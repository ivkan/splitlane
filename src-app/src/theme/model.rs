//! Terminal theme data model and UI palette.

use gpui::{Hsla, Rgba};

use crate::terminal::element::{MIN_APCA_CONTRAST, ensure_minimum_contrast};

/// Terminal color theme with an optional app-wide UI palette plus 36 terminal slots:
/// 5 base + cursor + selection + selection_foreground + scrollbar_thumb +
/// link_text + 2 title bar + 24 ANSI (8 hues x 3 intensities).
#[derive(Clone, Copy)]
pub struct TerminalTheme {
    /// Optional app-wide UI palette. Legacy themes derive their chrome colors
    /// from light/dark defaults; bundled custom themes can opt into exact UI
    /// tokens so the theme affects the whole app, not just ANSI colors.
    pub ui: Option<UiColors>,
    pub background: Hsla,
    pub foreground: Hsla,
    pub bright_foreground: Hsla,
    pub dim_foreground: Hsla,
    pub ansi_background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    /// Foreground color for text inside the selection rect,
    /// guaranteed to satisfy APCA Lc ≥ `MIN_APCA_CONTRAST` against
    /// `selection`. Computed once at theme-load time by
    /// [`TerminalTheme::recompute_selection_foreground`] from the theme's
    /// regular `foreground`. Used by `build_layout` to override the
    /// per-cell `fg` for cells inside the selection.
    pub selection_foreground: Hsla,
    pub scrollbar_thumb: Hsla,
    /// Color for hyperlink underline and text on Ctrl+hover.
    pub link_text: Hsla,
    pub title_bar_background: Hsla,
    pub title_bar_inactive_background: Hsla,
    // 8 hues x 3 intensities = 24 ANSI colors
    pub black: Hsla,
    pub red: Hsla,
    pub green: Hsla,
    pub yellow: Hsla,
    pub blue: Hsla,
    pub magenta: Hsla,
    pub cyan: Hsla,
    pub white: Hsla,
    pub bright_black: Hsla,
    pub bright_red: Hsla,
    pub bright_green: Hsla,
    pub bright_yellow: Hsla,
    pub bright_blue: Hsla,
    pub bright_magenta: Hsla,
    pub bright_cyan: Hsla,
    pub bright_white: Hsla,
    pub dim_black: Hsla,
    pub dim_red: Hsla,
    pub dim_green: Hsla,
    pub dim_yellow: Hsla,
    pub dim_blue: Hsla,
    pub dim_magenta: Hsla,
    pub dim_cyan: Hsla,
    pub dim_white: Hsla,
    /// Per-language syntax-highlighting colors for the diff view
    /// A dedicated semantic
    /// palette - NOT the 8-hue ANSI set above - so diff syntax can mirror the
    /// coverage of a modern editor's `SyntaxTheme` (≈18 distinct hues).
    /// `Copy`, snapshotted once per diff load via `DiffSyntax::from_theme`.
    pub syntax: SyntaxPalette,
}

/// Semantic syntax-highlighting palette for the diff view, mirroring the
/// *structure and coverage* of Zed's `SyntaxTheme` (a `name → color` map) in
/// Splitlane's Catppuccin brand. Each slot is one tree-sitter capture family;
/// `diff/syntax.rs::color_for_capture` resolves a capture name to a slot with
/// longest-prefix fallback. Color-only for v1 (no font-style); `Copy` and
/// allocation-free (~30 × `Hsla`).
#[derive(Clone, Copy)]
pub struct SyntaxPalette {
    pub comment: Hsla,
    pub comment_doc: Hsla,
    pub keyword: Hsla,
    pub function: Hsla,
    pub r#type: Hsla,
    pub r#enum: Hsla,
    pub constructor: Hsla,
    pub string: Hsla,
    pub string_escape: Hsla,
    pub string_special: Hsla,
    pub number: Hsla,
    pub boolean: Hsla,
    pub constant: Hsla,
    pub constant_builtin: Hsla,
    pub property: Hsla,
    pub variable: Hsla,
    pub variable_builtin: Hsla,
    pub operator: Hsla,
    pub punctuation: Hsla,
    pub punctuation_special: Hsla,
    pub attribute: Hsla,
    pub tag: Hsla,
    pub label: Hsla,
    pub namespace: Hsla,
    pub title: Hsla,
    pub text_literal: Hsla,
    pub link_uri: Hsla,
    pub link_text: Hsla,
    pub emphasis: Hsla,
    pub emphasis_strong: Hsla,
}

impl SyntaxPalette {
    /// Dark diff syntax palette (`one_dark()`), tuned from the Codex App diff
    /// reference while keeping the existing semantic slot structure. ≥ 18
    /// distinct values.
    pub fn catppuccin_mocha() -> Self {
        Self {
            comment: h(0x989898),
            comment_doc: h(0xa0a0a0),
            keyword: h(0xb070ff),
            function: h(0xa868e8),
            r#type: h(0xf89850),
            r#enum: h(0xf0a060),
            constructor: h(0xf8a858),
            string: h(0x40c878),
            string_escape: h(0x70c8f0),
            string_special: h(0xf87878),
            number: h(0xf8c060),
            boolean: h(0xf0b858),
            constant: h(0xf8d878),
            constant_builtin: h(0x70c8f0),
            property: h(0xf0a060),
            variable: h(0xf89850),
            variable_builtin: h(0xf8a858),
            operator: h(0x70c8f0),
            punctuation: h(0xd8d0d0),
            punctuation_special: h(0xf87878),
            attribute: h(0x78d0f8),
            tag: h(0xf87070),
            label: h(0xf0c8b8),
            namespace: h(0xa868e8),
            title: h(0xff8080),
            text_literal: h(0x48d080),
            link_uri: h(0x70c8f0),
            link_text: h(0xb070ff),
            emphasis: h(0xf08090),
            emphasis_strong: h(0xf0c8b8),
        }
    }

    /// Harbor Dark's syntax palette.
    ///
    /// The design specifies diff line colors and the tool-call blue but says
    /// nothing about code highlighting, so this is authored: Harbor's own
    /// four hues (accent green, tool blue, warning amber, danger clay) fanned
    /// into the thirty capture families at the low chroma the rest of the
    /// theme keeps. Structure carries the blues, values the greens, literals
    /// the ambers, and anything destructive or attention-seeking the clay.
    pub fn harbor_dark() -> Self {
        Self {
            comment: h(0x61666e),
            comment_doc: h(0x71767f),
            keyword: h(0xb49ac8),
            function: h(0x8fa8c8),
            r#type: h(0x79c6c0),
            r#enum: h(0x8ecfc9),
            constructor: h(0xafc2da),
            string: h(0x5ecfa8),
            string_escape: h(0xa8e5cf),
            string_special: h(0xd9a6c0),
            number: h(0xe0b25f),
            boolean: h(0xeccb8c),
            constant: h(0xd6c08a),
            constant_builtin: h(0xc8b27a),
            property: h(0xa4bcd6),
            // A step above the pane's body text rather than equal to it: a
            // variable is the thing being named, and it has to be legible as
            // that against the surrounding punctuation.
            variable: h(0xdadfe5),
            variable_builtin: h(0xd98a72),
            operator: h(0x9aa0a8),
            punctuation: h(0x8b9098),
            punctuation_special: h(0xb9bfc7),
            attribute: h(0x9dc0bd),
            tag: h(0xe6a794),
            label: h(0xc4b39a),
            namespace: h(0xa294b8),
            title: h(0xe6e8ea),
            text_literal: h(0x88d9bb),
            link_uri: h(0x6fb8d4),
            link_text: h(0x5ecfa8),
            emphasis: h(0xd6b8a8),
            emphasis_strong: h(0xefd3b4),
        }
    }

    /// Harbor Light's syntax palette.
    ///
    /// The mirror of [`Self::harbor_dark`], and authored for the same reason:
    /// the design gives the light theme diff inks and a tool-call blue and
    /// says nothing about code highlighting. Same four families - accent
    /// green, tool blue, warning amber, danger clay - turned into dark inks
    /// that hold their hue on white, at the low chroma the rest of the theme
    /// keeps.
    pub fn harbor_light() -> Self {
        Self {
            comment: h(0x8b908c),
            comment_doc: h(0x7a7f7b),
            keyword: h(0x7c5a97),
            function: h(0x3d6392),
            r#type: h(0x16706b),
            r#enum: h(0x1f7d77),
            constructor: h(0x4f7aad),
            string: h(0x157f5f),
            string_escape: h(0x14603f),
            string_special: h(0x97527a),
            number: h(0x9a6b12),
            boolean: h(0x85601b),
            constant: h(0x7a5510),
            constant_builtin: h(0x6d5a20),
            property: h(0x46688f),
            // A step below the pane's body text rather than equal to it - the
            // dark palette's reasoning, pointed the other way.
            variable: h(0x24272b),
            variable_builtin: h(0xb3462f),
            operator: h(0x5a5f5c),
            punctuation: h(0x6a6f6c),
            punctuation_special: h(0x3f4442),
            attribute: h(0x2f7a6b),
            tag: h(0xa4432c),
            label: h(0x6f5a3a),
            namespace: h(0x6b5486),
            title: h(0x1b1d1c),
            text_literal: h(0x1a7355),
            link_uri: h(0x2a6f8e),
            link_text: h(0x157f5f),
            emphasis: h(0x8a5a48),
            emphasis_strong: h(0x6b4a1e),
        }
    }

    /// Catppuccin Latte - the light-theme syntax palette. Darker, saturated
    /// hues that read on the white editor surface; ≥ 18 distinct values.
    pub fn catppuccin_latte() -> Self {
        Self {
            comment: h(0x9ca0b0),             // Overlay0
            comment_doc: h(0x8c8fa1),         // Overlay1
            keyword: h(0x8839ef),             // Mauve
            function: h(0x1e66f5),            // Blue
            r#type: h(0x179299),              // Teal
            r#enum: h(0x179299),              // Teal
            constructor: h(0x1e66f5),         // Blue
            string: h(0x40a02b),              // Green
            string_escape: h(0x04a5e5),       // Sky
            string_special: h(0xea76cb),      // Pink
            number: h(0xfe640b),              // Peach
            boolean: h(0xfe640b),             // Peach
            constant: h(0xdf8e1d),            // Yellow
            constant_builtin: h(0x209fb5),    // Sapphire
            property: h(0xd20f39),            // Red
            variable: h(0x4c4f69),            // Text
            variable_builtin: h(0xfe640b),    // Peach
            operator: h(0x04a5e5),            // Sky
            punctuation: h(0x5c5f77),         // Subtext1
            punctuation_special: h(0xe64553), // Maroon
            attribute: h(0x1e66f5),           // Blue
            tag: h(0xd20f39),                 // Red
            label: h(0xdc8a78),               // Rosewater
            namespace: h(0x7287fd),           // Lavender
            title: h(0xd20f39),               // Red
            text_literal: h(0x40a02b),        // Green
            link_uri: h(0x04a5e5),            // Sky
            link_text: h(0x1e66f5),           // Blue
            emphasis: h(0xe64553),            // Maroon
            emphasis_strong: h(0xdd7878),     // Flamingo
        }
    }

    /// All slots as a flat array - for tests counting distinct hues and for any
    /// future iteration over the palette.
    #[cfg(test)]
    pub(crate) fn all_slots(&self) -> [Hsla; 30] {
        [
            self.comment,
            self.comment_doc,
            self.keyword,
            self.function,
            self.r#type,
            self.r#enum,
            self.constructor,
            self.string,
            self.string_escape,
            self.string_special,
            self.number,
            self.boolean,
            self.constant,
            self.constant_builtin,
            self.property,
            self.variable,
            self.variable_builtin,
            self.operator,
            self.punctuation,
            self.punctuation_special,
            self.attribute,
            self.tag,
            self.label,
            self.namespace,
            self.title,
            self.text_literal,
            self.link_uri,
            self.link_text,
            self.emphasis,
            self.emphasis_strong,
        ]
    }
}

pub(super) fn h(hex: u32) -> Hsla {
    let r = ((hex >> 16) & 0xFF) as f32 / 255.0;
    let g = ((hex >> 8) & 0xFF) as f32 / 255.0;
    let b = (hex & 0xFF) as f32 / 255.0;
    Hsla::from(Rgba { r, g, b, a: 1.0 })
}

pub(super) fn ha(hex: u32, alpha: f32) -> Hsla {
    let mut color = h(hex);
    color.a = alpha;
    color
}

const CHROME_BACKGROUND_HEX: u32 = 0x141414;
// Shared right-panel background for terminals, tab bars, Diff, and Agents.
const TERMINAL_BACKGROUND_HEX: u32 = 0x181818;
const BORDER_HEX: u32 = 0x252525;

fn is_light_theme(theme: &TerminalTheme) -> bool {
    theme.background.l > 0.5
}

impl TerminalTheme {
    /// Recompute [`Self::selection_foreground`] from the current
    /// `foreground` and `selection` colors so APCA Lc(selection_fg, selection)
    /// ≥ [`MIN_APCA_CONTRAST`]. Called at theme-load time (and on every
    /// hot-reload). Reusing the same `ensure_minimum_contrast` algorithm as
    /// per-cell text guarantees consistent visual semantics - selected text
    /// is no harder to read than non-selected text on near-luminance themes.
    pub(crate) fn recompute_selection_foreground(&mut self) {
        // The `selection` slot's alpha represents how the selection blends
        // with the cell background underneath. For contrast purposes we
        // approximate the perceived selection background as the opaque
        // version of `selection` (alpha = 1.0). A future refinement could
        // alpha-composite against the actual cell `background` for a
        // tighter contrast estimate, but the simple opaque-bg model is
        // what `MIN_APCA_CONTRAST` was tuned for in the per-cell path.
        let selection_bg_opaque = Hsla {
            a: 1.0,
            ..self.selection
        };
        self.selection_foreground =
            ensure_minimum_contrast(self.foreground, selection_bg_opaque, MIN_APCA_CONTRAST);
    }
}

pub(super) fn apply_surface_overrides(mut theme: TerminalTheme) -> TerminalTheme {
    if theme.ui.is_some() || is_light_theme(&theme) {
        // Light themes skip surface overrides but still need their
        // selection_foreground populated; do it here so every theme exiting
        // this function has a valid value regardless of branch taken.
        theme.recompute_selection_foreground();
        return theme;
    }

    let chrome_bg = h(CHROME_BACKGROUND_HEX);
    let terminal_bg = h(TERMINAL_BACKGROUND_HEX);
    theme.title_bar_background = chrome_bg;
    theme.title_bar_inactive_background = chrome_bg;
    theme.background = terminal_bg;
    theme.ansi_background = terminal_bg;
    theme.foreground = h(0xf0f3f7);
    theme.bright_foreground = h(0xffffff);
    theme.dim_foreground = h(0x9ca7b5);
    theme.selection = ha(0x5aa6ff, 0.22);
    theme.scrollbar_thumb = ha(0x9aa8bd, 0.30);
    theme.link_text = h(0x57d5c4);
    theme.recompute_selection_foreground();
    theme
}

// ---------------------------------------------------------------------------
// UI color palette - derived from the active terminal theme
// ---------------------------------------------------------------------------

/// Blend two colors through RGBA. Straight-line in RGB rather than in HSL so a
/// blend between a near-grey surface and a saturated accent does not sweep the
/// hue wheel on the way.
/// Readable text on a solid fill of `fill`.
///
/// The fill is the background here, so the fill's own luminance decides and the
/// theme's does not. This is the rule [`UiColors::on_accent`] is built from,
/// exposed because `on_accent` is the answer for the **accent** fill and for no
/// other: borrowing it for a danger button paints a light theme's dark
/// on-accent text onto red.
pub fn text_on_fill(fill: Hsla) -> Hsla {
    if fill.l > 0.5 {
        mix(fill, h(0x000000), 0.86)
    } else {
        h(0xffffff)
    }
}

/// How far into a status hue a chip's **fill** goes: enough to be read as
/// tinted, not enough to be read as filled.
pub(super) const TINT_SURFACE: f32 = 0.14;
/// How far into a status hue a chip's **edge** goes - far enough to stay an
/// edge rather than become a highlight.
pub(super) const TINT_BORDER: f32 = 0.42;

/// The floor a non-text UI element has to clear against its ground. The same
/// 3 : 1 Harbor Light's focus border is held to.
pub(super) const UI_CONTRAST_FLOOR: f32 = 3.0;

/// WCAG relative luminance, through sRGB.
pub(super) fn relative_luminance(color: Hsla) -> f32 {
    let rgba = Rgba::from(color);
    let linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
}

/// WCAG contrast ratio between two opaque colours.
pub(super) fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The focused pane's border for a theme that states none.
///
/// It is not a fixed blend, and that is the point. On a dark theme nothing is
/// drawn beside it - `pane_border_idle` is transparent - so this one line is
/// the whole answer to "where do my keystrokes land", and it has to clear the
/// floor against **both** grounds it lies on: the pane's own fill, which it
/// borders for most of its length, and the desk showing in the gap between
/// panes.
///
/// `mix(base, accent, 0.25)` stood here and cleared neither on One Dark
/// (1.76 : 1 on the fill), and no fixed factor can, because how far a blend
/// gets depends on the accent: One Dark's green reaches the floor around 0.6
/// and a saturated blue never does at any ratio. So the blend walks up until
/// it clears, and an accent that cannot carry the signal hands it to the
/// theme's own `text`, which is readable on its own `base` by construction.
fn derived_focus_border(base: Hsla, accent: Hsla, desk: Hsla, text: Hsla) -> Hsla {
    let clears = |candidate: Hsla| {
        contrast_ratio(candidate, base) >= UI_CONTRAST_FLOOR
            && contrast_ratio(candidate, desk) >= UI_CONTRAST_FLOOR
    };
    // Toward the accent first: a focus border that keeps the theme's own hue
    // is the one the design draws, and the smallest blend that clears is the
    // quietest border that still answers.
    for step in 0..=15 {
        let candidate = mix(base, accent, 0.25 + 0.05 * step as f32);
        if clears(candidate) {
            return candidate;
        }
    }
    // The accent cannot get there. Walk toward the text instead - a colourless
    // border that can be seen beats a tinted one that cannot.
    for step in 1..=20 {
        let candidate = mix(base, text, 0.05 * step as f32);
        if clears(candidate) {
            return candidate;
        }
    }
    text
}

fn mix(from: Hsla, to: Hsla, ratio: f32) -> Hsla {
    let from = Rgba::from(from);
    let to = Rgba::from(to);
    let ratio = ratio.clamp(0.0, 1.0);
    Hsla::from(Rgba {
        r: from.r + (to.r - from.r) * ratio,
        g: from.g + (to.g - from.g) * ratio,
        b: from.b + (to.b - from.b) * ratio,
        a: from.a + (to.a - from.a) * ratio,
    })
}

/// The eight roles a theme states about itself.
///
/// Everything else in [`UiColors`] can be blended from these - see
/// [`UiColors::from_core`] - which is what keeps the Harbor roles from costing
/// five themes fifteen invented hex values each. A theme that has an opinion
/// about one of the blended roles simply states it.
#[derive(Clone, Copy)]
pub struct CoreRoles {
    pub base: Hsla,
    pub surface: Hsla,
    pub overlay: Hsla,
    pub border: Hsla,
    pub subtle: Hsla,
    pub muted: Hsla,
    pub text: Hsla,
    pub accent: Hsla,
}

/// Colors for the app chrome (sidebar, settings, badges, etc.).
#[derive(Clone, Copy)]
pub struct UiColors {
    /// Use the theme's `vc_*` washes directly for the Diff surface. Default
    /// dark themes keep Splitlane's historical opaque Codex-style diff washes;
    /// custom bundled themes opt into their own line backgrounds.
    pub use_theme_diff_washes: bool,
    pub base: Hsla,    // deepest background (settings sidebar, app bg)
    pub surface: Hsla, // card/panel background
    pub overlay: Hsla, // dropdown/popover bg
    pub border: Hsla,  // borders, dividers
    pub subtle: Hsla,  // hover bg, badge bg
    pub muted: Hsla,   // secondary text, labels
    pub text: Hsla,    // primary text
    pub accent: Hsla,  // active indicator, highlighted items
    // ---------------------------------------------------------------
    // Harbor roles. The eight above are the ones every theme has always
    // stated; these are the distinctions the Harbor design draws that they
    // could not express - six surface levels instead of three, four border
    // weights instead of one, and a seven-step text ramp instead of two.
    // A theme states them or lets `UiColors::from_core` blend them from the
    // eight above (see `CoreRoles`).
    // ---------------------------------------------------------------
    /// The desk the window sits on - visible only where the window does not
    /// reach, and behind a modal scrim.
    pub desk: Hsla,
    /// The gutter between panes: darker than a pane so the split reads as a
    /// gap rather than as a border.
    pub gutter: Hsla,
    /// Title bar, project toolbar and status bar - the app's own frame, as
    /// opposed to the content it frames.
    pub chrome: Hsla,
    /// A hairline inside a surface: the rule under a header, between two
    /// groups of one list. Lighter than `border`, which separates surfaces.
    pub divider: Hsla,
    /// A border that has to carry weight on its own: a focused field, a
    /// selected card.
    pub border_strong: Hsla,
    /// A dialog's outer border - the one border drawn over a scrim.
    pub border_dialog: Hsla,
    /// A control's border under the pointer.
    pub border_hover: Hsla,
    /// The fill of a project group's label in the rail.
    ///
    /// Its own role because no existing fill does the job. It has to sit a
    /// step above `subtle` (hover) and the active row's fill, or a project row
    /// under the pointer reads as another label; and it must not be a
    /// control's fill, because the label is a heading, not a button. Harbor
    /// Dark's value is also its `border_strong`, but that role is a border
    /// and this one is a fill.
    pub tag_fill: Hsla,
    /// The rule down a project group's members, 2px at the rail's left.
    /// Harbor's value in both themes is `border_hover`'s: the design picked
    /// the dark one to match it and asked for the light one to follow.
    pub group_rule: Hsla,
    /// The border around the slot that takes input. The one border in the app
    /// that means "your keystrokes land here", which is why it is a role of
    /// its own rather than the accent: focus is a state, not an accent.
    pub focus_border: Hsla,
    /// What an **unfocused** pane edge draws, so that focus reads as a change
    /// in the same place at the same weight rather than as one hairline
    /// appearing on bare ground.
    ///
    /// The answer is not the same in both halves of the world, and measuring
    /// is what says so. On Harbor Light the pane fill and the desk behind it
    /// sit **1.23 : 1** apart, so an unfocused pane has no edge of its own and
    /// one has to be drawn; `border_strong` against `focus_border` measures
    /// 3.08 : 1 and that pair is the fix. On Harbor Dark the same move
    /// puts a second line at **1.25 : 1** of the focused one - two
    /// near-identical hairlines the eye has to compare rather than an outline
    /// it can find - which is why the design's dark mockup draws
    /// `transparent` here and why this build now does too.
    pub pane_border_idle: Hsla,
    /// The row a keyboard-navigated list has landed on - the launcher's list
    /// and the `\u{2318}K` palette.
    ///
    /// One step above `subtle`, which is hover, plus a 2px accent bar on the
    /// leading edge drawn by the row itself. Both halves are load-bearing:
    /// while the highlight was `subtle` it was **the same colour as hover**,
    /// so a selection and a pointer resting on some other row were
    /// indistinguishable - and a background alone is not enough to tell them
    /// apart at arm's length even when the two tones differ.
    pub list_selection: Hsla,
    /// The header over a slot that is not taking input.
    pub slot_header: Hsla,
    /// The header over the slot that is - one step forward, so focus reads as
    /// a lift of the header rather than as a tint of it.
    pub slot_header_focus: Hsla,
    /// Long-form prose: an assistant's answer, a paragraph of help text. One
    /// step below `text`, which is for names and labels the eye lands on.
    pub text_body: Hsla,
    /// A row's second line, a field's value, anything read after the name.
    pub text_secondary: Hsla,
    /// Command output and other text the app did not write itself.
    pub text_tertiary: Hsla,
    /// Below `muted`: a hint the user reads once, a key chord beside a row.
    pub dim: Hsla,
    /// The faintest readable step - an inert affordance, a disabled label.
    pub faint: Hsla,
    /// A tinted surface that says "this is the accent's business": the
    /// selected broadcast row, an active toggle's fill.
    pub accent_surface: Hsla,
    /// The border of that surface.
    pub accent_border: Hsla,
    /// Text drawn on top of a solid `accent` fill.
    pub on_accent: Hsla,
    /// The window's own drop shadow, where the app draws it rather than the
    /// compositor.
    ///
    /// Elevation is the one part of the design that is *not* identical
    /// between the two themes: the design drops every shadow and the scrim
    /// by roughly a factor of four on light, because a dark shadow on white
    /// reads as dirt rather than as height. The geometry stays put - blur and
    /// offset are in [`crate::ui_tokens::shadow`] - and only the colour is
    /// the theme's, which is why these are roles and not constants.
    pub shadow_window: Hsla,
    /// A dialog's drop shadow.
    pub shadow_dialog: Hsla,
    /// A menu's or popover's drop shadow.
    pub shadow_menu: Hsla,
    /// The veil a modal is drawn over. Not black on light: the design tints
    /// it with the theme's own grey so the page underneath greys out instead
    /// of going muddy.
    pub scrim: Hsla,
    /// Distinct background for the `WaitingForConfirmation` tool-card
    /// header.
    /// Mirrors Zed's `tool_card_header_bg` -- an accent-tinted variant
    /// of the card surface that signals "this row is actionable" at a
    /// glance without redrawing the whole card.
    pub tool_card_header_bg: Hsla,
    // Curated version-control
    // colors for the Git Diff surface, mirroring Zed's `StatusColors`
    // model (`crates/theme/src/styles/status.rs`) - first-class slots,
    // NOT terminal-ANSI-derived. Light/dark variants are resolved in
    // `ui_colors_with`. `vc_*` are the foreground (status icons, file
    // labels, hunk gutter); `*_background` default to the foreground at
    // 0.25 alpha (Zed's `*_background` convention) for line washes;
    // `vc_word_*` are the stronger intra-line word-diff emphasis.
    /// Added / created (green).
    pub vc_added: Hsla,
    /// Modified / changed (yellow).
    pub vc_modified: Hsla,
    /// Deleted / removed (red).
    pub vc_deleted: Hsla,
    /// Merge conflict (orange - distinct from delete-red).
    pub vc_conflict: Hsla,
    /// Added-line background wash.
    pub vc_added_background: Hsla,
    /// Deleted-line background wash.
    pub vc_deleted_background: Hsla,
    /// Modified-line background wash.
    pub vc_modified_background: Hsla,
    /// Intra-line word-diff emphasis (added side).
    pub vc_word_added: Hsla,
    /// Intra-line word-diff emphasis (deleted side).
    pub vc_word_deleted: Hsla,
    // Broadcast-group
    // stripe palette - eight first-class slots so render code never inlines a
    // hex. Positional identity colors (not semantic status colors):
    // they only need to stay mutually distinguishable and readable as a 3px
    // pane-edge stripe on both bundled themes.
    pub group_1: Hsla,
    pub group_2: Hsla,
    pub group_3: Hsla,
    pub group_4: Hsla,
    pub group_5: Hsla,
    pub group_6: Hsla,
    pub group_7: Hsla,
    pub group_8: Hsla,
    // Agent terminal-state
    // slots (no inline hex in render code). Both are deliberately
    // distinct from `vc_conflict` (the attention/waiting dot) so a crashed
    // agent never reads as "needs input".
    /// `AgentState::Errored` - tab dot + sidebar badge (red).
    pub agent_error: Hsla,
    /// `AgentState::Stalled` - sidebar badge (muted grey-blue:
    /// "silent", not "failing").
    pub agent_stalled: Hsla,
    // Per-tool identity colors, promoted from the inline
    // hexes the sidebar spinner rows used. Brand hues, identical
    // on both themes by design (they tint text on the theme surface).
    /// Claude rows/spinner (Anthropic salmon).
    pub agent_claude: Hsla,
    /// Codex rows/spinner (Codex indigo).
    pub agent_codex: Hsla,
}

/// Effective version-control diff colors for the Git Diff / Review surfaces.
///
/// On dark themes the foreground plus the line and gutter washes are the
/// Codex-app-sampled green/red, a deliberate override of the muted `vc_*` theme
/// slots (which read too desaturated on the dense diff body). On light themes
/// they fall through to the theme `vc_*` slots. Single source for the Agents
/// diff dock, the Diff/Review view, and the diff sidebar so the three never
/// drift.
#[derive(Clone, Copy)]
pub struct DiffColors {
    pub added: Hsla,
    pub deleted: Hsla,
    pub added_background: Hsla,
    pub deleted_background: Hsla,
    pub added_gutter_background: Hsla,
    pub deleted_gutter_background: Hsla,
}

impl UiColors {
    /// The chip fill for a status hue, by the same blend `accent_surface` is.
    ///
    /// It is a method rather than two more slots on the struct because a theme
    /// that overrides `agent_error` (both bundled light palettes do) would then
    /// carry a surface blended off the *default* hue - a pair that cannot drift
    /// is worth more here than a pair that can be themed apart, and no theme
    /// has ever asked to.
    pub fn tinted_surface(&self, hue: Hsla) -> Hsla {
        mix(self.base, hue, TINT_SURFACE)
    }

    /// The chip edge for a status hue, by the same blend `accent_border` is.
    pub fn tinted_border(&self, hue: Hsla) -> Hsla {
        mix(self.base, hue, TINT_BORDER)
    }

    /// A full palette from the eight roles a theme states, with the Harbor
    /// roles and the status/identity slots blended from them.
    ///
    /// Meant to be used as the tail of a struct-update literal: a theme writes
    /// the slots it has an opinion about and lets this fill the rest. That is
    /// deliberately the *permissive* direction - a role added later starts out
    /// blended everywhere instead of failing five palettes to compile - because
    /// a blended step of a ramp is a defensible value, while a hex invented to
    /// satisfy the compiler is not.
    pub fn from_core(core: CoreRoles) -> Self {
        let CoreRoles {
            base,
            surface,
            overlay,
            border,
            subtle,
            muted,
            text,
            accent,
        } = core;
        let is_light = base.l > 0.5;
        let black = h(0x000000);
        // The desk is *behind* the window, so it goes away from the content in
        // whichever direction the theme has room: down on a dark theme, and
        // barely at all on a light one, where going down would read as a
        // second, darker window rather than as a backdrop.
        let (desk_shade, gutter_shade) = if is_light { (0.06, 0.03) } else { (0.45, 0.22) };

        Self {
            use_theme_diff_washes: false,
            base,
            surface,
            overlay,
            border,
            subtle,
            muted,
            text,
            accent,
            desk: mix(base, black, desk_shade),
            gutter: mix(base, black, gutter_shade),
            chrome: mix(base, overlay, 0.5),
            divider: mix(border, base, 0.35),
            border_strong: mix(border, text, 0.08),
            border_dialog: mix(border, text, 0.16),
            border_hover: mix(border, text, 0.24),
            tag_fill: mix(subtle, text, 0.06),
            group_rule: mix(border, text, 0.24),
            // Far enough into the accent to be read as "here", far enough from
            // it to stay a border rather than a highlight - and measured
            // rather than guessed at a fixed ratio, see the function.
            focus_border: derived_focus_border(base, accent, mix(base, black, desk_shade), text),
            // A theme that states nothing gets the answer its own half of the
            // world needs - see the field's own note for the measurements.
            pane_border_idle: if is_light {
                mix(border, text, 0.08)
            } else {
                ha(0x000000, 0.0)
            },
            // A step off hover, in whichever direction the theme's own text
            // lies: on a dark theme that lifts it, on a light one it deepens.
            list_selection: mix(subtle, text, 0.04),
            slot_header: mix(base, overlay, 0.5),
            slot_header_focus: mix(mix(base, overlay, 0.5), text, 0.05),
            text_body: mix(text, muted, 0.30),
            text_secondary: mix(text, muted, 0.50),
            text_tertiary: mix(text, muted, 0.75),
            dim: mix(muted, base, 0.25),
            faint: mix(muted, base, 0.45),
            accent_surface: mix(base, accent, TINT_SURFACE),
            accent_border: mix(base, accent, TINT_BORDER),
            // Text on a solid accent fill: the accent's own luminance decides,
            // not the theme's, because the fill is the background here.
            on_accent: text_on_fill(accent),
            // A theme that states nothing about elevation gets the design's
            // two sets, picked by which half of the world it lives in.
            shadow_window: if is_light {
                ha(0x181a18, 0.10)
            } else {
                ha(0x000000, 0.55)
            },
            shadow_dialog: if is_light {
                ha(0x181a18, 0.12)
            } else {
                ha(0x000000, 0.60)
            },
            shadow_menu: if is_light {
                ha(0x181a18, 0.10)
            } else {
                ha(0x000000, 0.55)
            },
            scrim: if is_light {
                ha(0x5a5f5c, 0.24)
            } else {
                ha(0x08090a, 0.60)
            },
            tool_card_header_bg: mix(surface, accent, 0.10),
            vc_added: mix(accent, h(0x57d992), 0.5),
            vc_modified: h(0xffd166),
            vc_deleted: h(0xff6f6a),
            vc_conflict: h(0xffa657),
            vc_added_background: ha(0x57d992, if is_light { 0.16 } else { 0.12 }),
            vc_deleted_background: ha(0xff6f6a, if is_light { 0.16 } else { 0.12 }),
            vc_modified_background: ha(0xffd166, if is_light { 0.16 } else { 0.12 }),
            vc_word_added: ha(0x57d992, 0.40),
            vc_word_deleted: ha(0xff6f6a, 0.40),
            group_1: accent,
            group_2: h(0x7eb6ff),
            group_3: h(0x57d992),
            group_4: h(0xffd166),
            group_5: h(0xc79bff),
            group_6: h(0x57d5c4),
            group_7: h(0xffa657),
            group_8: mix(text, muted, 0.5),
            agent_error: h(0xff6f6a),
            agent_stalled: muted,
            agent_claude: h(0xffa657),
            agent_codex: h(0x7eb6ff),
        }
    }

    /// The app's own frame - title bar, project toolbar, status bar, and the
    /// rails - dimmed one step while the window is not focused.
    ///
    /// This is the one place the chrome fill is decided. It used to be read
    /// straight off `TerminalTheme::title_bar_background`, a *terminal* slot:
    /// a theme could state the whole Harbor palette and still not reach the
    /// surface the design names, because that slot answered first. The role is
    /// the source now, and the terminal slot only paints the terminal.
    ///
    /// The inactive step goes toward `base` rather than to a second stated
    /// colour: a theme states what it has an opinion about, and "the window
    /// behind the one you are using" is not an opinion any theme has had to
    /// hold.
    pub fn chrome_for(&self, is_window_active: bool) -> Hsla {
        if is_window_active {
            self.chrome
        } else {
            mix(self.chrome, self.base, 0.55)
        }
    }

    /// Resolve the canonical diff color set (see [`DiffColors`]). Dark/light is
    /// keyed off `base.l` so any render path holding a `UiColors` can call it
    /// without re-locking the theme cache.
    pub fn diff_colors(&self) -> DiffColors {
        if self.base.l > 0.5 || self.use_theme_diff_washes {
            return DiffColors {
                added: self.vc_added,
                deleted: self.vc_deleted,
                added_background: self.vc_added_background,
                deleted_background: self.vc_deleted_background,
                added_gutter_background: self.vc_added_background,
                deleted_gutter_background: self.vc_deleted_background,
            };
        }
        DiffColors {
            // Codex-inspired dark diff panel, softened for the blue-black shell.
            added: h(0x57d992),
            deleted: h(0xff6f6a),
            added_background: h(0x1d3a2b),
            deleted_background: h(0x402425),
            added_gutter_background: h(0x16281f),
            deleted_gutter_background: h(0x2c1718),
        }
    }

    /// Stripe color for broadcast-group slot `idx` (0-based). Wraps modulo 8
    /// so an out-of-range index (impossible via the picker, which caps group
    /// creation at 8) can never panic the render path.
    pub fn group_color(&self, idx: usize) -> Hsla {
        match idx % 8 {
            0 => self.group_1,
            1 => self.group_2,
            2 => self.group_3,
            3 => self.group_4,
            4 => self.group_5,
            5 => self.group_6,
            6 => self.group_7,
            _ => self.group_8,
        }
    }
}

/// Derive UI colors from the active terminal theme.
///
/// Light themes get light UI; dark themes get dark UI. Calls
/// [`ui_colors_with`] under the hood after a single theme lookup --
/// render paths that already have a `TerminalTheme` in hand should
/// call [`ui_colors_with`] directly to avoid re-locking the theme
/// cache (Composer / ThreadView render do this).
pub fn ui_colors() -> UiColors {
    let theme = super::watcher::active_theme();
    ui_colors_with(&theme)
}

/// Derive UI colors from an already-resolved terminal theme. Identical
/// output to [`ui_colors`] but skips the global theme-cache lock --
/// the agents UI calls `ui_colors` once per render entry and again
/// for every visible item, so passing the cached theme through saves
/// O(visible_items) mutex acquisitions per frame.
pub fn ui_colors_with(theme: &TerminalTheme) -> UiColors {
    if let Some(ui) = theme.ui {
        return ui;
    }

    let is_light = is_light_theme(theme);
    if is_light {
        UiColors {
            use_theme_diff_washes: false,
            // Light theme: a slightly warmer surface with a faint
            // accent tint so the awaiting-confirmation row stands
            // out from neutral card surfaces without overwhelming
            // the chat stream.
            tool_card_header_bg: h(0xeff1f8),
            // Curated diff palette (Catppuccin Latte family) - darker,
            // saturated hues that read on a light surface.
            vc_added: h(0x40a02b),
            vc_modified: h(0xdf8e1d),
            vc_deleted: h(0xd20f39),
            vc_conflict: h(0xfe640b),
            // Subtle line wash (Zed editor_diff_hunk_*_background, light a=0x29=0.16);
            // the opaque gutter hunk bar carries the strong status signal.
            vc_added_background: ha(0x40a02b, 0.16),
            vc_deleted_background: ha(0xd20f39, 0.16),
            vc_modified_background: ha(0xdf8e1d, 0.16),
            vc_word_added: ha(0x40a02b, 0.40),
            vc_word_deleted: ha(0xd20f39, 0.40),
            // Broadcast stripes (Catppuccin Latte family) - saturated hues
            // that hold up as a thin stripe on a light pane edge.
            group_1: h(0x1e66f5),
            group_2: h(0x40a02b),
            group_3: h(0xdf8e1d),
            group_4: h(0xd20f39),
            group_5: h(0x8839ef),
            group_6: h(0x179299),
            group_7: h(0xfe640b),
            group_8: h(0x7287fd),
            // Agent state (Latte family): saturated red for a crash, the
            // neutral overlay grey for a silent session.
            agent_error: h(0xd20f39),
            agent_stalled: h(0x7c7f93),
            agent_claude: h(0xe89271),
            agent_codex: h(0x5b6cff),
            // Codex-style light shell: the right-hand work area is pure white,
            // while controls use cool, near-white layers for hierarchy. The
            // Harbor roles blend off these.
            ..UiColors::from_core(CoreRoles {
                base: h(0xffffff),
                surface: h(0xf7f7f9),
                overlay: h(0xffffff),
                border: h(0xe5e5ed),
                subtle: h(0xedeef2),
                muted: h(0x686a73),
                text: h(0x25262b),
                accent: h(0x4c6fff),
            })
        }
    } else {
        UiColors {
            use_theme_diff_washes: false,
            // Dark theme: a touch lighter and bluer than the card
            // surface so the accent character of the
            // awaiting row reads even at a glance.
            tool_card_header_bg: h(0x2a2e3a),
            // Premium dark diff palette: Codex-like red/green intent,
            // softened to sit inside the blue-black terminal surface.
            vc_added: h(0x57d992),
            vc_modified: h(0xffd166),
            vc_deleted: h(0xff6f6a),
            vc_conflict: h(0xffa657),
            // Subtle line wash (Zed editor_diff_hunk_*_background, dark a=0x1f=0.12);
            // the opaque gutter hunk bar carries the strong status signal.
            vc_added_background: ha(0x57d992, 0.12),
            vc_deleted_background: ha(0xff6f6a, 0.12),
            vc_modified_background: ha(0xffd166, 0.12),
            vc_word_added: ha(0x57d992, 0.40),
            vc_word_deleted: ha(0xff6f6a, 0.40),
            // Broadcast stripes: high-luminance accents that keep their
            // identity against the new blue-black pane edge.
            group_1: h(0x7eb6ff),
            group_2: h(0x57d992),
            group_3: h(0xffd166),
            group_4: h(0xff6f6a),
            group_5: h(0xc79bff),
            group_6: h(0x57d5c4),
            group_7: h(0xffa657),
            group_8: h(0x9ea7ff),
            // Agent state: clear, bright marks on the muted cockpit shell.
            agent_error: h(0xff6f6a),
            agent_stalled: h(0x96a2b3),
            agent_claude: h(0xffa657),
            agent_codex: h(0x7eb6ff),
            // The cockpit shell every legacy dark theme falls back to; the
            // Harbor roles blend off it.
            ..UiColors::from_core(CoreRoles {
                base: h(TERMINAL_BACKGROUND_HEX),
                surface: h(0x212121),
                overlay: h(CHROME_BACKGROUND_HEX),
                border: h(BORDER_HEX),
                subtle: h(0x2a2a2a),
                muted: h(0x96a2b3),
                text: h(0xd5deea),
                accent: h(0x57d5c4),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::element::apca_contrast;
    use crate::theme::builtin::{THEMES, harbor_dark, harbor_light, one_dark, theme_by_name};

    /// The bug this role exists to fix: the chrome used to be painted from
    /// `title_bar_background`, a terminal slot, so a theme could restate the
    /// whole chrome palette and change nothing on screen. Switching themes has
    /// to move the chrome, and the light theme has to land on a light one.
    #[test]
    fn the_chrome_follows_the_theme_and_not_the_terminal_slot() {
        let chromes: Vec<Hsla> = THEMES
            .iter()
            .map(|(_, build)| ui_colors_with(&build()).chrome_for(true))
            .collect();
        assert!(
            chromes.windows(2).any(|pair| pair[0] != pair[1]),
            "every bundled theme paints the same chrome - the role is not reaching it"
        );
        assert!(ui_colors_with(&harbor_light()).chrome_for(true).l > 0.5);

        // An unfocused window dims its frame, and both steps stay opaque:
        // chrome is a fill, never a veil over whatever is behind the window.
        let ui = ui_colors_with(&harbor_dark());
        assert_ne!(ui.chrome_for(true), ui.chrome_for(false));
        assert_eq!(ui.chrome_for(false).a, 1.0);
    }

    #[test]
    fn light_theme_keeps_light_surfaces_after_overrides() {
        let theme = apply_surface_overrides(harbor_light());

        assert!(theme.background.l > 0.5);
        assert_eq!(theme.background, h(0xffffff));
        assert!(theme.ansi_background.l > 0.5);
        assert!(theme.title_bar_background.l > 0.5);
    }

    /// The subject was Splitlane Light until that theme was removed; Harbor Light
    /// keeps the invariant, and its own border literal is not repeated here -
    /// the theme declares it, so the assertion asks the theme.
    #[test]
    fn light_ui_keeps_the_work_area_pure_white() {
        let declared = harbor_light().ui.expect("Harbor Light states its palette");
        let ui = ui_colors_with(&harbor_light());

        assert_eq!(ui.base, h(0xffffff));
        assert_eq!(ui.overlay, h(0xffffff));
        assert_eq!(ui.border, declared.border);
        assert_ne!(ui.surface, ui.base);
        assert_ne!(ui.border, ui.base);
        assert_ne!(ui.text, ui.base);
    }

    #[test]
    fn dark_theme_still_uses_dark_surface_overrides() {
        let theme = apply_surface_overrides(one_dark());

        assert_eq!(theme.background.l, h(TERMINAL_BACKGROUND_HEX).l);
        assert_eq!(theme.ansi_background.l, h(TERMINAL_BACKGROUND_HEX).l);
        assert_eq!(theme.title_bar_background.l, h(CHROME_BACKGROUND_HEX).l);
    }

    #[test]
    fn dark_ui_uses_cockpit_surface_palette() {
        let ui = ui_colors_with(&one_dark());

        assert_eq!(ui.base, h(TERMINAL_BACKGROUND_HEX));
        assert_eq!(ui.overlay, h(CHROME_BACKGROUND_HEX));
        assert_eq!(ui.border, h(BORDER_HEX));
    }

    /// A theme that states its own app-wide palette keeps it, rather than
    /// falling back to the default's, once `apply_surface_overrides` has run.
    ///
    /// It used to run over Vercel, Claude and Cursor - the three custom dark
    /// themes - and all three were removed. The invariant did not: Harbor
    /// Light carries it now, because it is the one remaining theme that both
    /// **states** a `ui` block and is **not** the default, which is exactly the
    /// pair the invariant needs. One Dark cannot stand in: it declares no
    /// palette of its own and derives one from the core roles, so it has
    /// nothing to keep.
    ///
    /// Asserted against the palette the theme itself declares rather than
    /// against literals, so the check cannot rot the next time a colour moves.
    #[test]
    fn a_theme_that_states_its_own_palette_keeps_it() {
        let stated = harbor_light();
        let declared = stated
            .ui
            .expect("Harbor Light states its own palette - that is why it is the subject");
        let theme = apply_surface_overrides(harbor_light());
        let ui = ui_colors_with(&theme);

        assert_eq!(ui.base, declared.base, "base");
        assert_eq!(ui.surface, declared.surface, "surface");
        assert_eq!(ui.accent, declared.accent, "accent");
        assert_eq!(
            ui.use_theme_diff_washes, declared.use_theme_diff_washes,
            "diff wash mode"
        );
    }

    /// Invariant: for any theme exiting `apply_surface_overrides` or
    /// `theme_by_name`, the selection foreground must satisfy the same
    /// APCA Lc threshold the per-cell contrast pass uses. A near-luminance
    /// theme without this invariant would render selected text illegibly.
    fn assert_selection_invariant(theme: &TerminalTheme, label: &str) {
        let bg_opaque = Hsla {
            a: 1.0,
            ..theme.selection
        };
        let lc = apca_contrast(theme.selection_foreground, bg_opaque).abs();
        assert!(
            lc >= MIN_APCA_CONTRAST,
            "{label}: APCA Lc({lc}) < {MIN_APCA_CONTRAST} for selection_foreground vs selection"
        );
    }

    #[test]
    fn bundled_themes_satisfy_selection_contrast_invariant() {
        // All bundled themes must produce a readable selection foreground.
        // Iterate the live table so a newly bundled theme is covered the
        // moment it is registered, not the next time someone edits this list.
        for (label, build) in THEMES {
            assert_selection_invariant(&apply_surface_overrides(build()), label);
        }
    }

    #[test]
    fn theme_by_name_returns_invariant_satisfying_themes() {
        // theme_by_name is the public entry point; users may call it without
        // going through apply_surface_overrides, so it must finalize on the
        // way out. Iterate the live table so the test tracks the bundled set.
        for (name, _) in crate::theme::builtin::THEMES {
            let theme = theme_by_name(name).expect("bundled theme not found");
            assert_selection_invariant(&theme, name);
        }
    }

    #[test]
    fn adversarial_selection_close_to_red_text_still_legible() {
        // Construct a synthetic theme whose `selection` background is a
        // strong red, the same hue as theme.foreground, then assert the
        // recomputed selection_foreground still satisfies the invariant.
        // This is the canonical "user picked a clashing selection color"
        // failure mode this invariant must guard against.
        let mut theme = one_dark();
        theme.foreground = h(0xff0000); // bright red text
        theme.selection = ha(0xff0000, 0.4); // selection of the same hue
        theme.recompute_selection_foreground();
        assert_selection_invariant(&theme, "adversarial-red-on-red");
    }

    #[test]
    fn adversarial_selection_close_to_white_on_light_theme() {
        // Light theme + near-white selection background. The algorithm must
        // pick a dark foreground for legibility.
        let mut theme = harbor_light();
        theme.foreground = h(0xeeeeee); // near-white text
        theme.selection = ha(0xf0f0f0, 0.5); // very pale selection
        theme.recompute_selection_foreground();
        assert_selection_invariant(&theme, "adversarial-white-on-light");
    }

    #[test]
    fn vc_diff_slots_distinct_with_subtle_zed_alpha_backgrounds() {
        // The curated diff slots are
        // distinct hues. The line-wash backgrounds are subtle (Zed's
        // editor_diff_hunk_*_background: 0.12 dark / 0.16 light) - the opaque
        // gutter hunk bar carries the strong status signal, not the wash.
        let dark = ui_colors_with(&one_dark());
        assert_ne!(dark.vc_added, dark.vc_deleted);
        assert_ne!(dark.vc_added, dark.vc_modified);
        assert_ne!(dark.vc_deleted, dark.vc_modified);
        for bg in [
            dark.vc_added_background,
            dark.vc_deleted_background,
            dark.vc_modified_background,
        ] {
            assert!(
                (bg.a - 0.12).abs() < 1e-6,
                "dark diff background alpha must be 0.12, got {}",
                bg.a
            );
        }
        // The light theme resolves a distinct palette, and its backgrounds are
        // **opaque** rather than washes.
        //
        // The alpha here used to be asserted at 0.16, which was Splitlane
        // Light's mechanism - a translucent wash over white - and that theme
        // was removed. Harbor Light paints the three states as solid
        // fills instead, so the property worth holding is not an alpha but the
        // one the alpha was serving: three diff states a reader can tell apart,
        // none of them the surface they sit on.
        let light = ui_colors_with(&harbor_light());
        assert_ne!(light.vc_added, dark.vc_added);
        let light_backgrounds = [
            light.vc_added_background,
            light.vc_deleted_background,
            light.vc_modified_background,
        ];
        for bg in light_backgrounds {
            assert_ne!(bg, light.base, "a diff background must not be the surface");
        }
        for (i, a) in light_backgrounds.iter().enumerate() {
            for b in light_backgrounds.iter().skip(i + 1) {
                assert_ne!(a, b, "the three diff states must be distinguishable");
            }
        }

        // Both branches of `diff_colors` keep a live subject after Vercel,
        // Claude and Cursor were taken out. The rule under test never
        // depended on those three - only on the two flags it reads.
        //
        // A theme that opts into its own washes gets them.
        let own = ui_colors_with(&harbor_dark());
        assert!(own.use_theme_diff_washes);
        let diff = own.diff_colors();
        assert_eq!(diff.added, own.vc_added);
        assert_eq!(diff.deleted, own.vc_deleted);
        assert_eq!(diff.added_background, own.vc_added_background);

        // A dark theme that does not gets the canonical dark palette instead -
        // One Dark declares no `ui` block at all, so its colours are derived
        // and the flag stays off, which is exactly the case Claude and Cursor
        // used to stand for.
        let derived = ui_colors_with(&one_dark());
        assert!(!derived.use_theme_diff_washes);
        assert!(derived.base.l <= 0.5, "the fallback branch is dark-only");
        let diff = derived.diff_colors();
        let canonical_dark_diff = dark.diff_colors();
        assert_eq!(diff.added, canonical_dark_diff.added);
        assert_eq!(diff.deleted, canonical_dark_diff.deleted);
        assert_eq!(diff.added_background, canonical_dark_diff.added_background);
        assert_eq!(
            diff.deleted_background,
            canonical_dark_diff.deleted_background
        );
    }

    #[test]
    fn harbor_states_every_role_the_design_draws() {
        // The design's colour list is a set of *distinctions*: six surface
        // levels, four border weights, seven steps of text. Blending them from
        // eight core roles - which is what every other theme does - would
        // collapse exactly the distinctions the design is made of, so Harbor
        // has to state them, and this is what says so.
        let ui = ui_colors_with(&harbor_dark());

        let surfaces = [
            ui.desk, ui.gutter, ui.base, ui.surface, ui.chrome, ui.overlay,
        ];
        assert_eq!(
            distinct_count(&surfaces),
            6,
            "Harbor states six surface levels"
        );
        // Darkest to lightest, in the order the design layers them.
        for pair in surfaces.windows(2) {
            assert!(
                pair[0].l < pair[1].l,
                "Harbor surfaces must climb: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }

        let borders = [
            ui.divider,
            ui.border,
            ui.border_strong,
            ui.border_dialog,
            ui.border_hover,
        ];
        assert_eq!(
            distinct_count(&borders),
            5,
            "Harbor states five border weights"
        );

        let ramp = [
            ui.text,
            ui.text_body,
            ui.text_secondary,
            ui.text_tertiary,
            ui.muted,
            ui.dim,
            ui.faint,
        ];
        assert_eq!(
            distinct_count(&ramp),
            7,
            "Harbor states a seven-step text ramp"
        );
        for pair in ramp.windows(2) {
            assert!(
                pair[0].l > pair[1].l,
                "the text ramp must descend: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }

        // The design's diff line backgrounds are opaque colours, not a wash
        // over the pane - and `use_theme_diff_washes` is what routes the Diff
        // surface to them instead of Splitlane's historical Codex greens.
        assert!(ui.use_theme_diff_washes);
        assert_eq!(ui.diff_colors().added_background, ui.vc_added_background);
        assert_eq!(ui.vc_added_background.a, 1.0);
        assert_eq!(ui.vc_deleted_background.a, 1.0);
    }

    #[test]
    fn harbor_light_states_every_role_the_design_draws() {
        // The dark half's test, pointed the other way - and deliberately not a
        // copy of it, because the light map is not the dark one inverted. Two
        // of its four decision groups change *shape*, not just value, and this
        // is what says which.
        let ui = ui_colors_with(&harbor_light());

        // Light gains a surface step over dark, and spends it in the middle of
        // the stack: the pane and a dialog are both white, and the layers under
        // them do the separating. So five distinct values, not six.
        //
        // The order is the second thing the light map does not inherit. On dark
        // the desk is the darkest thing on screen; on light the *gutter* is,
        // because the gutter still has to read as a gap between two white panes
        // while the desk only has to read as "not the window". Inverting the
        // dark stack would have swapped exactly these two.
        let surfaces = [
            ui.gutter, ui.desk, ui.chrome, ui.surface, ui.base, ui.overlay,
        ];
        assert_eq!(
            distinct_count(&surfaces),
            5,
            "Harbor Light states five surface values across six levels"
        );
        assert_eq!(ui.base, ui.overlay, "the pane and a dialog are both white");
        assert!(
            ui.gutter.l < ui.desk.l,
            "the gutter has to separate two white panes; the desk only has to sit behind one window"
        );
        // Darkest to lightest, in the order the design layers them, with white
        // at the top.
        for pair in surfaces.windows(2) {
            assert!(
                pair[0].l <= pair[1].l,
                "Harbor Light surfaces must climb: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }

        let borders = [
            ui.divider,
            ui.border,
            ui.border_strong,
            ui.border_dialog,
            ui.border_hover,
        ];
        assert_eq!(
            distinct_count(&borders),
            5,
            "Harbor Light states five border weights"
        );
        // On white a border earns weight by going *down*, which is the whole
        // reason the light map could not be the dark one flipped in place.
        for pair in borders.windows(2) {
            assert!(
                pair[0].l > pair[1].l,
                "Harbor Light borders must descend: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }

        let ramp = [
            ui.text,
            ui.text_body,
            ui.text_secondary,
            ui.text_tertiary,
            ui.muted,
            ui.dim,
            ui.faint,
        ];
        assert_eq!(
            distinct_count(&ramp),
            7,
            "Harbor Light states a seven-step text ramp"
        );
        for pair in ramp.windows(2) {
            assert!(
                pair[0].l < pair[1].l,
                "the light text ramp must climb: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }

        // The accent is the decision the design made for us, and the one a
        // reader is most likely to "fix" back to an inversion later. The dark
        // accent on white is 1.9:1; this one has to clear 4.5:1, and the text
        // drawn on it has to go *lighter*, not darker.
        assert_eq!(ui.accent, h(0x157f5f));
        assert!(
            wcag_contrast(ui.accent, ui.base) >= 4.5,
            "the light accent fails on white: {:.2}:1",
            wcag_contrast(ui.accent, ui.base)
        );
        assert_eq!(ui.on_accent, h(0xffffff));
        assert!(ui.on_accent.l > ui.accent.l);

        // Same as the dark half: opaque line backgrounds, routed to the Diff
        // surface by `use_theme_diff_washes`.
        assert!(ui.use_theme_diff_washes);
        assert_eq!(ui.diff_colors().added_background, ui.vc_added_background);
        assert_eq!(ui.vc_added_background.a, 1.0);
        assert_eq!(ui.vc_deleted_background.a, 1.0);
        // "Pale washes with dark ink" - the wash is the light half of the pair
        // and the text on it the dark half, which is exactly what inverting
        // the dark theme's tints would have got backwards.
        assert!(ui.vc_added_background.l > ui.vc_added.l);
        assert!(ui.vc_deleted_background.l > ui.vc_deleted.l);

        // The three focus signals, stated rather than blended.
        assert_eq!(ui.focus_border, h(0x35785f));
        assert_eq!(ui.slot_header, h(0xf0f0ed));
        assert_eq!(ui.slot_header_focus, h(0xeef2f0));
    }

    /// WCAG 2.x contrast ratio. The design states the light accent's contrast
    /// in these terms ("4.8:1 on #ffffff"), so the assertion is written in them
    /// too - APCA is the terminal cell's measure, not this one's.
    fn wcag_contrast(a: Hsla, b: Hsla) -> f32 {
        fn luminance(color: Hsla) -> f32 {
            let rgba = Rgba::from(color);
            let channel = |c: f32| {
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
        }
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn a_theme_that_states_no_harbor_role_still_gets_a_full_ramp() {
        // `from_core` is the whole reason adding fifteen roles did not cost
        // five palettes fifteen invented hexes each. What it blends still has
        // to be usable: distinct steps, in order.
        for (label, build) in THEMES {
            let ui = ui_colors_with(&build());
            let ramp = [ui.text, ui.text_body, ui.text_secondary, ui.text_tertiary];
            assert_eq!(distinct_count(&ramp), 4, "{label}: text ramp collapsed");
            // The desk sits behind the window, so it never comes forward.
            // It may however arrive at the same colour: Vercel's base is pure
            // black and has nowhere darker to go, and a desk drawn *lighter*
            // than the window would read as a hole rather than as a backdrop.
            assert!(
                ui.desk.l <= ui.base.l,
                "{label}: the desk is lighter than the window"
            );
            assert!(
                ui.gutter.l <= ui.base.l,
                "{label}: the gutter is lighter than the pane"
            );
            if ui.base.l > 0.05 {
                assert_ne!(ui.desk, ui.base, "{label}: the desk equals the window");
            }
            assert_ne!(ui.divider, ui.border_hover, "{label}: borders collapsed");
        }
    }

    #[test]
    fn the_focused_slot_reads_differently_in_every_theme() {
        // Three signals fire together for the focused pane, and two of them
        // are colours: the border and the header fill. A theme that blends
        // them to the same value as their unfocused counterparts would leave
        // the rail's accent bar carrying focus alone.
        for (label, build) in THEMES {
            let ui = ui_colors_with(&build());
            assert_ne!(
                ui.slot_header, ui.slot_header_focus,
                "{label}: the focused header equals the unfocused one"
            );
            assert_ne!(
                ui.focus_border, ui.border,
                "{label}: the focus border is just a border"
            );
        }
        // Harbor states the design's own three values rather than blending
        // them.
        let harbor = ui_colors_with(&harbor_dark());
        assert_eq!(harbor.focus_border, h(0x44705f));
        assert_eq!(harbor.slot_header, h(0x1c1f23));
        assert_eq!(harbor.slot_header_focus, h(0x20242a));
    }

    use super::contrast_ratio as contrast;

    #[test]
    fn a_pane_edge_answers_focus_in_one_of_the_two_ways_that_work() {
        // "Which pane takes my keystrokes" can be answered by an outline
        // **appearing** - the unfocused edge draws nothing - or by that
        // outline **changing** - the unfocused edge draws too, far enough from
        // the focused one to be told apart at a glance. There is no third way,
        // and the shape that does not work is a second line the focused one
        // cannot be distinguished from: Harbor Dark shipped `border_strong`
        // beside `focus_border` at **1.25 : 1**, which is a pair of identical
        // hairlines, and the fix that put it there was measured on
        // Harbor Light (3.08 : 1) and carried across unmeasured.
        //
        // 3 : 1 is the bar a non-text element has to clear, and it is the same
        // number Harbor Light's focus border is held to.
        for (label, build) in THEMES {
            let ui = ui_colors_with(&build());
            if ui.pane_border_idle.a <= f32::EPSILON {
                // Nothing is drawn beside it, so the border answers alone and
                // has to clear the floor against the two grounds it lies on:
                // the pane's own fill for most of its length, and the desk
                // showing between the panes.
                let on_fill = contrast(ui.focus_border, ui.base);
                let on_desk = contrast(ui.focus_border, ui.desk);
                assert!(
                    on_fill >= 3.0 && on_desk >= 3.0,
                    "{label}: an outline that appears is the whole signal, and \
                     this one is {on_fill:.2} : 1 on the pane fill and \
                     {on_desk:.2} : 1 on the desk"
                );
                continue;
            }
            let pair = contrast(ui.focus_border, ui.pane_border_idle);
            assert!(
                pair >= 3.0,
                "{label}: a drawn idle pane edge that the focused one is only \
                 {pair:.2} : 1 from is a second hairline, not a signal"
            );
        }
    }

    #[test]
    fn recompute_is_idempotent() {
        // Running the recompute twice must yield the same value - guards
        // against accidental mutation of `foreground` or `selection` during
        // the algorithm.
        let mut theme = one_dark();
        theme.recompute_selection_foreground();
        let first = theme.selection_foreground;
        theme.recompute_selection_foreground();
        let second = theme.selection_foreground;
        assert_eq!(first, second);
    }

    // ------------------------------------------------------------------
    // The per-language syntax palette must be richly populated on bundled
    // themes and stay readable on the light theme.
    // ------------------------------------------------------------------

    /// Count pairwise-distinct colors. `Hsla` is `PartialEq` but neither `Eq`
    /// nor `Hash`, so a `HashSet` is out; O(n²) over 30 slots is trivial.
    fn distinct_count(colors: &[Hsla]) -> usize {
        let mut seen: Vec<Hsla> = Vec::new();
        for &c in colors {
            if !seen.contains(&c) {
                seen.push(c);
            }
        }
        seen.len()
    }

    #[test]
    fn bundled_themes_populate_at_least_18_distinct_syntax_hues() {
        // ≥ 18 distinct color values per theme (up from 8).
        for (label, build) in THEMES {
            let distinct = distinct_count(&build().syntax.all_slots());
            assert!(
                distinct >= 18,
                "{label}: syntax palette has only {distinct} distinct hues (< 18)"
            );
        }
    }

    #[test]
    fn no_syntax_slot_equals_default_or_foreground() {
        // Unhappy path: a slot left at the default `Hsla`
        // (transparent black) or equal to the theme foreground would render
        // that token family invisible / indistinguishable from plain text.
        let default = Hsla::default();
        for (label, build) in THEMES {
            let theme = build();
            for (i, slot) in theme.syntax.all_slots().iter().enumerate() {
                assert_ne!(*slot, default, "{label}: syntax slot #{i} left at default");
                assert_ne!(
                    *slot, theme.foreground,
                    "{label}: syntax slot #{i} equals foreground"
                );
            }
        }
    }

    #[test]
    fn light_theme_comment_and_punctuation_perceptibly_off_foreground() {
        // On the light theme, comment and punctuation must clear
        // a perceptible APCA margin from the row foreground (not just `!=`).
        let theme = harbor_light();
        for (slot_label, slot) in [
            ("comment", theme.syntax.comment),
            ("punctuation", theme.syntax.punctuation),
        ] {
            let lc = apca_contrast(slot, theme.foreground).abs();
            assert!(
                lc > 5.0,
                "Latte: {slot_label} too close to foreground (APCA Lc {lc:.1})"
            );
        }
    }

    #[test]
    fn latte_core_slots_distinct_and_clear_of_background() {
        // The highest-traffic families (comment / string /
        // keyword / operator) are mutually distinct and distinct from
        // foreground on the light theme; and NO family near-equals the light
        // editor background (a slot collapsing onto bg = invisible category).
        let theme = harbor_light();
        let p = theme.syntax;
        let core = [p.comment, p.string, p.keyword, p.operator];
        assert_eq!(
            distinct_count(&core),
            4,
            "Latte: comment/string/keyword/operator not mutually distinct"
        );
        for c in core {
            assert_ne!(c, theme.foreground, "Latte: core slot equals foreground");
        }
        for (i, slot) in p.all_slots().iter().enumerate() {
            let lc = apca_contrast(*slot, theme.background).abs();
            assert!(
                lc > 5.0,
                "Latte: syntax slot #{i} too close to background (APCA Lc {lc:.1})"
            );
        }
    }
}
