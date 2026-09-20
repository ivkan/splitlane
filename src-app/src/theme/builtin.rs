//! Bundled terminal themes and the registry used to look them up by name.

use super::model::{SyntaxPalette, TerminalTheme, UiColors, h, ha};

pub type ThemeEntry = (&'static str, fn() -> TerminalTheme);

/// The theme the app starts with when config says nothing.
pub const DEFAULT_THEME_NAME: &str = "Harbor Dark";

/// The light counterpart, used by the light/dark/system selector.
///
/// Harbor's own light map is built, so the light seat is Harbor's
/// too: one design, two renderings of it. `Splitlane Light` stays bundled and
/// stays pickable - it is a Codex-shaped theme of its own, not the light half
/// of this one.
pub const DEFAULT_LIGHT_THEME_NAME: &str = "Harbor Light";

/// Every theme this build ships.
///
/// **Three, and the count is the decision.** Four more were bundled - Splitlane
/// Light, Vercel, Claude, Cursor - and were removed. A theme named after
/// somebody else's product is a promise to keep following it, and that promise
/// was already broken (Vercel had drifted from its own palette) with no way to
/// keep it: their release cycle is not ours. `One Dark` survives for the
/// opposite reason - it stopped being an editor's theme long ago and became a
/// shared terminal vocabulary. `Splitlane Light` held the light seat until
/// Harbor Light shipped; the seat is taken and the role is over.
pub static THEMES: &[ThemeEntry] = &[
    ("Harbor Dark", harbor_dark),
    ("Harbor Light", harbor_light),
    ("One Dark", one_dark),
];

pub fn harbor_dark() -> TerminalTheme {
    let mut theme = TerminalTheme {
        ui: Some(harbor_dark_ui()),
        // "window & pane surface" - the terminal sits on the pane.
        background: h(0x17191c),
        foreground: h(0xcdd2d8),
        bright_foreground: h(0xe6e8ea),
        dim_foreground: h(0x71767f),
        ansi_background: h(0x17191c),
        cursor: h(0x5ecfa8),
        selection: ha(0x5ecfa8, 0.22),
        // Filled below before returning the public theme value.
        selection_foreground: gpui::Hsla::default(),
        scrollbar_thumb: ha(0x2b2e34, 0.90),
        link_text: h(0x5ecfa8),
        // "title bar, toolbar, status".
        title_bar_background: h(0x1c1f23),
        title_bar_inactive_background: h(0x1a1c20),
        // The ANSI set is the design's own hues extended into eight families:
        // its accent green, its warning amber, its danger clay, its tool-call
        // blue, plus a muted magenta and cyan built in the same low-chroma key
        // so terminal output never out-shouts the chrome around it.
        black: h(0x24272c),
        red: h(0xd98a72),
        green: h(0x5ecfa8),
        yellow: h(0xe0b25f),
        blue: h(0x8fa8c8),
        magenta: h(0xb49ac8),
        cyan: h(0x79c6c0),
        white: h(0xb9bfc7),
        bright_black: h(0x3a3f47),
        bright_red: h(0xe6a794),
        bright_green: h(0xa8e5cf),
        bright_yellow: h(0xeccb8c),
        bright_blue: h(0xafc2da),
        bright_magenta: h(0xcdbbdc),
        bright_cyan: h(0xa1dcd7),
        bright_white: h(0xe6e8ea),
        dim_black: h(0x1f2226),
        dim_red: h(0x9c6252),
        dim_green: h(0x439377),
        dim_yellow: h(0xa07f44),
        dim_blue: h(0x66788e),
        dim_magenta: h(0x806e8f),
        dim_cyan: h(0x568c88),
        dim_white: h(0x8b9098),
        syntax: SyntaxPalette::harbor_dark(),
    };
    theme.recompute_selection_foreground();
    theme
}

fn harbor_dark_ui() -> UiColors {
    UiColors {
        // Harbor states its own diff washes, so the Diff surface uses them
        // instead of Splitlane's historical Codex-sampled greens and reds.
        use_theme_diff_washes: true,
        base: h(0x17191c),
        surface: h(0x1a1c20),
        overlay: h(0x1f2226),
        border: h(0x2b2e34),
        subtle: h(0x24272c),
        // The design's "labels" step. Its "muted" and "faint" steps are `dim`
        // and `faint` below - three dim greys where the old palette had one.
        muted: h(0x8b9098),
        text: h(0xe6e8ea),
        accent: h(0x5ecfa8),
        desk: h(0x0e0f11),
        gutter: h(0x131518),
        chrome: h(0x1c1f23),
        divider: h(0x262a2f),
        border_strong: h(0x2f3339),
        border_dialog: h(0x34383f),
        border_hover: h(0x3a3f47),
        // "border 1px when focused, transparent otherwise", and the two header
        // fills that go with it.
        //
        // Raised from the design's own `#33453f`, on our
        // measurement: that value is 1.88 : 1 against the desk and 1.73 : 1
        // against the pane fill, under the 3 : 1 Harbor Light's focus border
        // is held to. `#44705f` clears the floor against **both** grounds - 3.40 and
        // 3.13 - which `accent_border #3f6a5c` does not (2.87 against the
        // fill, and the fill is what the border sits on for most of its
        // length). `#3f6a5c` also already means the hovered divider and the
        // composer's focused input row, and a focused pane is a stronger
        // statement than a hovered divider.
        focus_border: h(0x44705f),
        // Transparent, as the design's dark mockup draws it. An earlier fix gave
        // an unfocused pane a `border_strong` edge and that fix was measured
        // on Harbor **Light**, where it is worth 3.08 : 1; carried onto dark
        // unmeasured it put a second hairline at 1.25 : 1 of the focused one,
        // so the two pane edges on screen differed by nothing the eye can use
        // and "which pane takes my keystrokes" went back to being a question.
        pane_border_idle: ha(0x000000, 0.0),
        slot_header: h(0x1c1f23),
        list_selection: h(0x282c31),
        slot_header_focus: h(0x20242a),
        text_body: h(0xcdd2d8),
        text_secondary: h(0xb9bfc7),
        text_tertiary: h(0x9aa0a8),
        dim: h(0x71767f),
        faint: h(0x61666e),
        accent_surface: h(0x1d2724),
        accent_border: h(0x3f6a5c),
        on_accent: h(0x10221c),
        shadow_window: ha(0x000000, 0.55),
        shadow_dialog: ha(0x000000, 0.60),
        shadow_menu: ha(0x000000, 0.55),
        scrim: ha(0x08090a, 0.60),
        tool_card_header_bg: h(0x1d2724),
        vc_added: h(0x5ecfa8),
        vc_modified: h(0xe0b25f),
        vc_deleted: h(0xd98a72),
        // The design has two status hues - "warning (running, modified)" and
        // "danger (deletions, errors)" - and no third for a merge conflict.
        // Warning is the closer read: a conflict is work waiting on the user,
        // not a failure, and the same amber already means "needs you" on a
        // running session. It is a deliberate step off the design's letter,
        // and the only one in this palette.
        vc_conflict: h(0xe0b25f),
        // Opaque, not alpha: the design gives the diff body exact line
        // backgrounds (`#16241f` added, `#251a19` removed) rather than a wash
        // over the pane.
        vc_added_background: h(0x16241f),
        vc_deleted_background: h(0x251a19),
        vc_modified_background: ha(0xe0b25f, 0.12),
        vc_word_added: ha(0xa8e5cf, 0.40),
        vc_word_deleted: ha(0xe6b3a6, 0.40),
        // Broadcast stripes: the accent first, then hues held in the same
        // low-chroma key so eight stripes stay distinguishable without any of
        // them reading as an alert.
        group_1: h(0x5ecfa8),
        group_2: h(0x8fa8c8),
        group_3: h(0xe0b25f),
        group_4: h(0xd98a72),
        group_5: h(0xb49ac8),
        group_6: h(0x79c6c0),
        group_7: h(0xa8e5cf),
        group_8: h(0xb9bfc7),
        agent_error: h(0xd98a72),
        agent_stalled: h(0x71767f),
        agent_claude: h(0xd98a72),
        agent_codex: h(0x8fa8c8),
    }
}

/// Harbor Light - the same design rendered with the design's light palette.
///
/// "The palette is a token map, not an inversion." Four groups needed real
/// decisions and the design made them for us: light gains one more surface
/// step than dark (white has to stay the reading surface), the borders warm
/// up, the text ramp keeps its six roles, and the accent is **not** the dark
/// one flipped - `#5ecfa8` on white is 1.9:1, so light takes `#157f5f` (4.8:1)
/// and white as the on-accent text.
///
/// Everything else - geometry, type, sizes, motion, priority rules - is
/// identical between the two, which is why this file's job is only colour.
pub fn harbor_light() -> TerminalTheme {
    let mut theme = TerminalTheme {
        ui: Some(harbor_light_ui()),
        // "window & pane surface" - the terminal sits on the pane, and on
        // light that pane is white.
        background: h(0xffffff),
        foreground: h(0x2c2f2e),
        bright_foreground: h(0x1b1d1c),
        dim_foreground: h(0x7a7f7b),
        ansi_background: h(0xffffff),
        cursor: h(0x157f5f),
        // A shade under the dark theme's 0.22: the same alpha over white
        // reads heavier than it does over `#17191c`.
        selection: ha(0x157f5f, 0.18),
        // Filled below before returning the public theme value.
        selection_foreground: gpui::Hsla::default(),
        // The dark theme draws its thumb from `border`; the light `border`
        // (`#d2d3ce`) is invisible on white, so the thumb takes the hover
        // border weight instead. Authored - the design has no thumb value.
        scrollbar_thumb: ha(0xa9aaa3, 0.85),
        link_text: h(0x157f5f),
        // "title bar, toolbar, status".
        title_bar_background: h(0xf0f0ed),
        title_bar_inactive_background: h(0xf6f6f4),
        // Authored on the same principle as Harbor Dark's set - the design's
        // own four hues extended into eight families - but as dark inks on
        // white rather than light ones on near-black. The light mock has no
        // ANSI list of its own; what it does have (the traffic lights) keeps
        // its own colours in both themes and is not part of this set.
        black: h(0x2c2f2e),
        red: h(0xb3462f),
        green: h(0x157f5f),
        yellow: h(0x9a6b12),
        blue: h(0x3d6392),
        magenta: h(0x7c5a97),
        cyan: h(0x16706b),
        white: h(0x8b908c),
        // The bright ramp goes *darker* on a light theme, not lighter: on
        // white, contrast is gained by going down. `Splitlane Light` already
        // reads this way (`green` #50a14f, `bright_green` #3e8a3e), and the
        // greys are the exception in both - `bright_black` is the light step
        // of the black family and `bright_white` the dark end of the white.
        //
        // Two of these are the design's own inks rather than authored: the
        // markdown viewer draws inline code and fenced blocks in
        // `bright_green` over the added-line wash ("code is quoted, not
        // typed"), so `bright_green` has to be the added ink `#14603f` for
        // the light theme's code to land on the value the design states.
        // `bright_red` takes the removed ink for the symmetry.
        bright_black: h(0x5a5f5c),
        bright_red: h(0x8e2f1c),
        bright_green: h(0x14603f),
        bright_yellow: h(0x7a5510),
        bright_blue: h(0x2a4f76),
        bright_magenta: h(0x5f3f78),
        bright_cyan: h(0x0f5551),
        bright_white: h(0x1b1d1c),
        dim_black: h(0xa9aaa3),
        dim_red: h(0xcd8e7e),
        dim_green: h(0x6fae96),
        dim_yellow: h(0xb8a273),
        dim_blue: h(0x8ba0ba),
        dim_magenta: h(0xab9bbd),
        dim_cyan: h(0x7fb0ac),
        dim_white: h(0x9aa09b),
        syntax: SyntaxPalette::harbor_light(),
    };
    theme.recompute_selection_foreground();
    theme
}

fn harbor_light_ui() -> UiColors {
    UiColors {
        // Same reason as the dark half: the design states the diff's own line
        // backgrounds, so the Diff surface uses them.
        use_theme_diff_washes: true,
        base: h(0xffffff),
        surface: h(0xf6f6f4),
        // "controls, dialogs" - white again. Light gains a surface step over
        // dark by splitting the *middle* of the stack, not the top: `base` and
        // `overlay` share white and the layers below them do the separating.
        overlay: h(0xffffff),
        border: h(0xd2d3ce),
        // "hover".
        subtle: h(0xe9eae6),
        // The design's "labels" step, as in the dark half.
        muted: h(0x6a6f6c),
        text: h(0x1b1d1c),
        accent: h(0x157f5f),
        desk: h(0xe8e8e6),
        gutter: h(0xdedfdc),
        chrome: h(0xf0f0ed),
        divider: h(0xdcdcd8),
        border_strong: h(0xc6c7c1),
        border_dialog: h(0xbcbdb7),
        border_hover: h(0xa9aaa3),
        // Was `0x9fd0bd`, which measured **1.27 : 1** against the
        // panes area and 1.70 : 1 against a pane's own fill - less than half
        // the 3 : 1 a non-text control needs, and the reason "which pane is
        // active" could not be answered without moving the mouse. `0x35785f`
        // is 5.25 : 1 on the fill and 3.92 : 1 on the backdrop.
        //
        // The other half of that fix is not here: an unfocused pane now draws
        // `border_strong` instead of nothing, so the signal is a **change**
        // between two borders rather than one hairline appearing on bare
        // ground. The pair itself measures 3.08 : 1.
        focus_border: h(0x35785f),
        // Light keeps the drawn edge: white on `#e8e8e6` is 1.23 : 1, so an
        // unfocused pane has no shape at all without one.
        pane_border_idle: h(0xc6c7c1),
        slot_header: h(0xf0f0ed),
        list_selection: h(0xe4e9e5),
        slot_header_focus: h(0xeef2f0),
        text_body: h(0x2c2f2e),
        text_secondary: h(0x3f4442),
        text_tertiary: h(0x5a5f5c),
        dim: h(0x7a7f7b),
        faint: h(0x8b908c),
        accent_surface: h(0xe2f2ec),
        accent_border: h(0x7cbfa6),
        // The one place "just invert it" would have been wrong twice over: the
        // accent is darker on light, so the text on it goes lighter, not
        // darker.
        on_accent: h(0xffffff),
        // The factor of four. Same geometry, a quarter of the weight, and a
        // shadow tinted with the palette's own warm grey rather than black.
        shadow_window: ha(0x181a18, 0.10),
        shadow_dialog: ha(0x181a18, 0.12),
        shadow_menu: ha(0x181a18, 0.10),
        scrim: ha(0x5a5f5c, 0.24),
        tool_card_header_bg: h(0xe2f2ec),
        vc_added: h(0x157f5f),
        vc_modified: h(0x9a6b12),
        vc_deleted: h(0xb3462f),
        // Same deliberate step off the design's letter as the dark half: it
        // gives two status hues and no third for a merge conflict, and warning
        // is the closer read.
        vc_conflict: h(0x9a6b12),
        // Opaque, like the dark half - the design gives the diff body exact
        // line backgrounds rather than a wash over the pane. "Inverting the
        // tints gives mud": these are pale washes carrying dark ink.
        vc_added_background: h(0xe3f4ea),
        vc_deleted_background: h(0xfbe6e1),
        vc_modified_background: ha(0x9a6b12, 0.12),
        // The design's diff *inks* (`#14603f` / `#8e2f1c`), which are the
        // contrast-corrected variants of the two status hues for the washes
        // above - the same role the dark half gives `#a8e5cf` / `#e6b3a6`.
        // The alpha is authored: 0.40 of a dark ink over a pale wash is a
        // block, where 0.40 of a light one over a dark wash is an emphasis.
        vc_word_added: ha(0x14603f, 0.22),
        vc_word_deleted: ha(0x8e2f1c, 0.22),
        // Broadcast stripes: the dark half's list, ink for ink. A stripe on a
        // white pane edge has to be dark to be a stripe at all.
        group_1: h(0x157f5f),
        group_2: h(0x3d6392),
        group_3: h(0x9a6b12),
        group_4: h(0xb3462f),
        group_5: h(0x7c5a97),
        group_6: h(0x16706b),
        group_7: h(0x14603f),
        group_8: h(0x5a5f5c),
        agent_error: h(0xb3462f),
        agent_stalled: h(0x7a7f7b),
        agent_claude: h(0xb3462f),
        agent_codex: h(0x3d6392),
    }
}

pub fn one_dark() -> TerminalTheme {
    let mut theme = TerminalTheme {
        ui: None,
        background: h(0x282c34),
        // Keep One Dark surfaces while using the same softer terminal text
        // palette as Cursor and the Windows Terminal reference.
        foreground: h(0xc5d1cc),
        bright_foreground: h(0xf2eee8),
        dim_foreground: h(0x93938b),
        ansi_background: h(0x282c34),
        cursor: h(0x007aff),
        selection: ha(0x5aa6ff, 0.22),
        // Filled below before returning the public theme value.
        selection_foreground: gpui::Hsla::default(),
        scrollbar_thumb: ha(0x9aa8bd, 0.30),
        link_text: h(0x57d5c4),
        title_bar_background: h(0x21252b),
        title_bar_inactive_background: h(0x1b1f23),
        black: h(0x0f1a1d),
        red: h(0xd4555e),
        green: h(0x56b68a),
        yellow: h(0xd4a35c),
        blue: h(0x5b8fb4),
        magenta: h(0xb578b4),
        cyan: h(0x4dc4b0),
        white: h(0xb8c8c0),
        bright_black: h(0x3a4f4a),
        bright_red: h(0xe87878),
        bright_green: h(0x7ddba8),
        bright_yellow: h(0xe8c478),
        bright_blue: h(0x7cb3d4),
        bright_magenta: h(0xd09acf),
        bright_cyan: h(0x72dbc9),
        bright_white: h(0xe0ece6),
        dim_black: h(0x2c313a),
        dim_red: h(0xb85659),
        dim_green: h(0x3f9d74),
        dim_yellow: h(0xb4934b),
        dim_blue: h(0x5a82b8),
        dim_magenta: h(0x906cb8),
        dim_cyan: h(0x3d9b91),
        dim_white: h(0x93938b),
        syntax: SyntaxPalette::catppuccin_mocha(),
    };
    theme.recompute_selection_foreground();
    theme
}

pub fn theme_by_name(name: &str) -> Option<TerminalTheme> {
    let name_lower = name.to_lowercase();
    THEMES
        .iter()
        .find(|(n, _)| n.to_lowercase() == name_lower)
        .map(|(_, f)| {
            let mut theme = f();
            theme.recompute_selection_foreground();
            theme
        })
}
