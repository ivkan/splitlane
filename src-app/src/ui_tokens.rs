//! The design tokens: one home for every UI size the app draws with.
//!
//! Colors are not here - they live in [`crate::theme::UiColors`], which a theme
//! can replace at runtime. Everything a theme does *not* get to change - type
//! scale, mono scale, corner radii, the spacing step, row heights, elevation
//! and motion - is here, named by the role it plays rather than by its value.
//!
//! # Why by role
//!
//! The scales come from the design ("Design tokens" in its README) and
//! are deliberately fine-grained: 13px and 12.5px are one step apart and mean
//! different things. Mapping a call site by nearest number instead of by role
//! flattens exactly the hierarchy the scale exists to carry - a 13px heading
//! and a 13px caption are not the same token, and rounding both to "13" loses
//! which one is allowed to move when the scale changes. So every constant below
//! is named for what it is used *for*, and a new call site picks the name, not
//! the number.
//!
//! # What is deliberately not in here
//!
//! - **Terminal cell rendering.** Its font family and size are the user's
//!   (`splitlane.json#font_family` / `font_size`); sweeping it into a UI scale
//!   would take a setting away from the user.
//! - **Layout geometry** - sidebar widths, minimum window size, split ratios.
//!   Those are composition, not typography, and none of them appear in the
//!   design's token list.

// A scale is a closed set: the design states seven UI sizes, six mono sizes
// and eight radii, and the tests below assert exactly those lists. A step that
// no call site happens to use today is still part of the scale - deleting it
// would leave the next call site to invent a number instead of picking a name.
#![allow(dead_code)]

use gpui::{Pixels, px};

/// Font families the chrome draws with.
///
/// The design asks for Instrument Sans + JetBrains Mono. JetBrains Mono ships
/// in `assets/fonts/` already; the sans is **Geist**, also already shipped, and
/// the design explicitly allows the substitution ("the layout survives a swap
/// to any neutral grotesque, the tokens do not change"). Adding a third family
/// would cost a license review, binary size and one more thing to ship, for a
/// difference the design itself calls optional.
pub mod font {
    /// The chrome's sans family: labels, rows, buttons, headings.
    ///
    /// The same constant the terminal's own font resolver uses for the
    /// `.SplitlaneSans` alias - naming the family twice is how the two drift.
    pub const UI: &str = crate::terminal::element::EMBEDDED_SANS_FAMILY;

    /// Code, paths, keys, counters and section labels - everything the design
    /// calls "code and metadata", which is exactly the JetBrains Mono it asks
    /// for and the app already ships.
    ///
    /// Naming the family beats asking for the generic `"monospace"`: the face
    /// is embedded and registered at boot, so this resolves identically on all
    /// three platforms, whereas the generic name is a fontconfig convention
    /// that Windows has no answer for.
    pub const MONO: &str = crate::terminal::element::EMBEDDED_MONO_FAMILY;
}

/// UI type scale (sans): 19 / 15 / 13.5 / 13 / 12.5 / 12 / 11.5.
pub mod text {
    use super::*;

    /// The single largest string in the app: an empty state's headline, the
    /// About dialog's product name. One per screen at most.
    pub const DISPLAY: Pixels = px(19.);

    /// A settings section's title - the heading of a whole page of content.
    pub const HEADING: Pixels = px(15.);

    /// A dialog's or overlay's title: "Broadcast", "Launch pad", "Add project".
    pub const TITLE: Pixels = px(13.5);

    /// Prose and command rows: a dialog's explanatory paragraph, a palette
    /// command, the active project's name in the toolbar.
    pub const BODY: Pixels = px(13.);

    /// The workhorse. List rows, menu items, buttons, tabs, session names -
    /// most text in the chrome is this size.
    pub const ROW: Pixels = px(12.5);

    /// A dense control: a small button, a close glyph, a chip inside a row.
    pub const CONTROL: Pixels = px(12.);

    /// A field label, a hint under a control, a secondary line under a row.
    pub const CAPTION: Pixels = px(11.5);
}

/// Mono type scale: 12 / 11.5 / 11 / 10.5 / 10 / 9.5.
///
/// Everything the design calls "code and metadata" - transcript bodies, paths,
/// branch names, key chords, counters and the uppercase section labels.
pub mod mono {
    use super::*;

    /// A transcript body, a diff line, a code block - running code the user
    /// reads line by line.
    pub const BODY: Pixels = px(12.);

    /// Inline code inside prose, and a path shown as a field's value.
    pub const INLINE: Pixels = px(11.5);

    /// A row's own mono text: a session's name, a branch, a key chord, a glyph
    /// standing in for an icon.
    pub const ROW: Pixels = px(11.);

    /// A path shown as a subtitle or a breadcrumb, and the `⌘K` affordance.
    pub const PATH: Pixels = px(10.5);

    /// The uppercase section label (`PROJECTS`, `FILES`, `UNCOMMITTED`) at
    /// weight 500, and the `+ agent` / `+ shell` affordances beside it.
    ///
    /// The design originally set 0.14em tracking on these; a later correction
    /// removed all tracking from the design because GPUI has no such property.
    pub const LABEL: Pixels = px(10.);

    /// The smallest thing drawn: a counter, a badge, a one-word hint under a
    /// control.
    pub const HINT: Pixels = px(9.5);
}

/// Corner radii: 12 window & dialogs · 9 menus · 8 panes & primary buttons ·
/// 7 controls & rows · 6 small controls · 5 tags · 4 badges · 3 meters.
pub mod radius {
    use super::*;

    /// The window itself, and any dialog drawn over it.
    pub const WINDOW: Pixels = px(12.);

    /// A dropdown or context menu.
    pub const MENU: Pixels = px(9.);

    /// A pane, a panel, and the primary button of a dialog.
    pub const PANEL: Pixels = px(8.);

    /// The focus ring that sits **one pixel outside** a [`PANEL`].
    ///
    /// Not a second opinion about how round a pane is - it is `PANEL` plus that
    /// one pixel, which is what keeps the two arcs concentric. A ring offset
    /// outward and given the same radius cuts *inside* the pane's own corner:
    /// the two curves diverge and the panes area shows through the gap. Measured
    /// on Harbor Light, 27 August 2026 - the seam was the loudest thing about
    /// the corner and read as "the focus border does not fit the pane".
    pub const PANEL_RING: Pixels = px(9.);

    /// The corner mask that clears a [`PANEL`]'s square content **inside** the
    /// reserved 1px border band.
    ///
    /// `PANEL` minus that pixel, and for the same reason `PANEL_RING` is
    /// `PANEL` plus it: all three arcs have to share a centre. The mask sits at
    /// a 1px inset, so giving it `PANEL`'s own radius puts its centre a pixel
    /// down and right of the pane's, and the two arcs cross - a white crescent
    /// of raw content shows between the mask and the focus ring at every
    /// corner. Measured on Harbor Light, 27 August 2026.
    pub const PANEL_INNER: Pixels = px(7.);

    /// A control, and a selectable row in a list.
    pub const CONTROL: Pixels = px(7.);

    /// A small control: an icon button, a toggle, a chip with a click target.
    pub const SMALL: Pixels = px(6.);

    /// A tag - a non-interactive label with a background.
    pub const TAG: Pixels = px(5.);

    /// A badge: a count, a status pill, the app mark.
    pub const BADGE: Pixels = px(4.);

    /// A meter, a progress track, a thin stripe.
    pub const METER: Pixels = px(3.);
}

/// The spacing step: 4 / 6 / 8 / 10 / 12 / 14 / 18 / 22 / 26 / 34.
///
/// Wider than a strict 4px grid on purpose - the design's own scale. Values
/// below 4 stay out of it because they are physical, not rhythmic: a border's
/// thickness, a hairline, an optical nudge on a glyph.
pub mod space {
    use super::*;

    /// Between a glyph and its label; the gap inside a chip.
    pub const XS: Pixels = px(4.);
    /// Between two tightly related controls.
    pub const SM: Pixels = px(6.);
    /// The default gap between siblings in a row.
    pub const MD: Pixels = px(8.);
    /// A row's horizontal padding in a dense list.
    pub const LG: Pixels = px(10.);
    /// A panel's inner padding; the gap between grouped rows.
    pub const XL: Pixels = px(12.);
    /// A header's horizontal padding.
    pub const XXL: Pixels = px(14.);
    /// The gap between two sections of a panel.
    pub const SECTION: Pixels = px(18.);
    /// A dialog's inner padding.
    pub const DIALOG: Pixels = px(22.);
    /// The gap between two blocks of a dialog.
    pub const BLOCK: Pixels = px(26.);
    /// A page's outer padding, and the widest gap the design uses.
    pub const PAGE: Pixels = px(34.);
}

/// Row heights: 44 toolbar · 40 title bar · 34 pane header, buttons, dialog
/// rows · 30 search · 28 project row & status bar · 25 session row.
pub mod row {
    use super::*;

    /// The project toolbar above the panes.
    pub const TOOLBAR: Pixels = px(44.);

    /// The window's title bar (client-side decorations only).
    pub const TITLE_BAR: Pixels = px(40.);

    /// A pane header, a button, a dialog's own rows.
    pub const HEADER: Pixels = px(34.);

    /// A search or filter field.
    pub const SEARCH: Pixels = px(30.);

    /// A container row in the rail, and the status bar.
    pub const PROJECT: Pixels = px(28.);

    /// A surface row in the rail - the densest row in the app.
    pub const SESSION: Pixels = px(25.);
}

/// Elevation, as `(y offset, blur radius)` in points.
///
/// The **colour** is deliberately not here. Everything else in this file is a
/// value no theme gets to change; a shadow is not, because the design drops
/// every shadow and the scrim by roughly a factor of four on the light theme -
/// a dark shadow on white reads as dirt, not as height. So the geometry, which
/// *is* identical between the two, stays a token, and the colour is a role:
/// `UiColors::shadow_window` / `shadow_dialog` / `shadow_menu`. Build the
/// `BoxShadow` with [`crate::ui_primitives::window_shadow`] and friends rather
/// than pairing the two by hand.
pub mod shadow {
    /// The window: `0 40px 100px`.
    pub const WINDOW: (f32, f32) = (40., 100.);
    /// A dialog: `0 30px 70px`.
    pub const DIALOG: (f32, f32) = (30., 70.);
    /// A menu or popover: `0 18px 44px`.
    pub const MENU: (f32, f32) = (18., 44.);
}

/// Motion. The design allowed exactly two animations and said so in as many
/// words: "caret rotate 120ms; skeleton pulse 1.4s ease-in-out infinite.
/// Nothing else animates." A later change adds a third, deliberately and with
/// its own argument - the announcement of the attention queue's edge - and
/// with it the scale's first easing curves.
///
/// # Three conditions on anything added here
///
/// - **Named by the work, not by the shape.** Never `EASE_OUT_EXPO`: a curve
///   named after its shape becomes universal, and the next animation then picks
///   it by taste instead of by argument.
/// - **Two curves are a pair belonging to one animation.** The next animation
///   either reuses [`ANNOUNCE_RISE`] / [`ANNOUNCE_SETTLE`] - which means it is
///   the same act - or argues for its own pair. That keeps this list a record
///   of decisions rather than a box of tools.
/// - **A token lands in the same commit that uses it.** A token with no call
///   site is exactly what the closed-list rule exists to catch.
///
/// The lists below are closed to arbitrary additions, which is not the same as
/// immutable: `every_token_is_on_the_design_scale` asserts the set, so growing
/// it is a decision somebody has to write down here.
pub mod motion {
    use std::time::Duration;

    /// A disclosure caret turning between collapsed and expanded.
    pub const CARET: Duration = Duration::from_millis(120);

    /// One period of a skeleton placeholder's pulse.
    pub const SKELETON_PULSE: Duration = Duration::from_millis(1400);

    /// One announcement that the attention queue has gone from empty to not.
    ///
    /// **Both ends of the number are argued, and the argument outlives it.**
    /// Below ~300 ms a peripheral event is *detected but not localised* - the
    /// reader notices "something" and cannot tell where, which sends them
    /// hunting, the exact cost this app refuses to impose. Above ~600 ms it starts to
    /// read as a cycle and the eye waits for a second beat. 480 ms is long
    /// enough to register, localise and finish.
    pub const ANNOUNCE: Duration = Duration::from_millis(480);

    /// The announcement's rise: the dot growing out of rest.
    pub const ANNOUNCE_RISE: Curve = Curve::new(0.16, 1.0, 0.3, 1.0);

    /// The announcement's settle: the dot coming back to rest.
    pub const ANNOUNCE_SETTLE: Curve = Curve::new(0.33, 1.0, 0.68, 1.0);

    /// A CSS-shaped cubic bezier, `cubic-bezier(x1, y1, x2, y2)`, with the two
    /// endpoints fixed at (0, 0) and (1, 1).
    ///
    /// GPUI ships `linear`, `quadratic`, `ease_in_out`, `ease_out_quint` and no
    /// way to state a curve by control points, and the design states these two
    /// that way - so the solver lives here beside the tokens rather than at the
    /// one call site that needs it.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Curve {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    }

    impl Curve {
        pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
            Self { x1, y1, x2, y2 }
        }

        /// The curve's `y` for a given `x` in `0..=1`.
        ///
        /// `x` is time and `y` is progress, so the parameter `t` behind both
        /// has to be recovered from `x` first - which is what a browser does
        /// too. Newton converges in a couple of steps for these two curves;
        /// bisection is the fallback for the flat regions where the derivative
        /// approaches zero, so the answer is bounded rather than merely
        /// usually right.
        pub fn ease(&self, x: f32) -> f32 {
            let x = x.clamp(0.0, 1.0);
            if x <= 0.0 || x >= 1.0 {
                return x;
            }
            let bezier = |a: f32, b: f32, t: f32| {
                let u = 1.0 - t;
                3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
            };
            let slope = |a: f32, b: f32, t: f32| {
                let u = 1.0 - t;
                3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
            };
            let mut t = x;
            for _ in 0..8 {
                let dx = bezier(self.x1, self.x2, t) - x;
                if dx.abs() < 1e-5 {
                    return bezier(self.y1, self.y2, t);
                }
                let d = slope(self.x1, self.x2, t);
                if d.abs() < 1e-6 {
                    break;
                }
                t -= dx / d;
            }
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            let mut t = x;
            for _ in 0..24 {
                let dx = bezier(self.x1, self.x2, t) - x;
                if dx.abs() < 1e-5 {
                    break;
                }
                if dx > 0.0 {
                    hi = t;
                } else {
                    lo = t;
                }
                t = (lo + hi) / 2.0;
            }
            bezier(self.y1, self.y2, t)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design states each scale as a closed list. A token that drifts off
    /// it is a silent divergence from the design, so the lists are asserted
    /// rather than trusted.
    #[test]
    fn every_token_is_on_the_design_scale() {
        let text_scale = [19., 15., 13.5, 13., 12.5, 12., 11.5];
        for value in [
            text::DISPLAY,
            text::HEADING,
            text::TITLE,
            text::BODY,
            text::ROW,
            text::CONTROL,
            text::CAPTION,
        ] {
            assert!(
                text_scale.contains(&f32::from(value)),
                "{value:?} off the UI scale"
            );
        }

        let mono_scale = [12., 11.5, 11., 10.5, 10., 9.5];
        for value in [
            mono::BODY,
            mono::INLINE,
            mono::ROW,
            mono::PATH,
            mono::LABEL,
            mono::HINT,
        ] {
            assert!(
                mono_scale.contains(&f32::from(value)),
                "{value:?} off the mono scale"
            );
        }

        let radius_scale = [12., 9., 8., 7., 6., 5., 4., 3.];
        for value in [
            radius::WINDOW,
            radius::MENU,
            radius::PANEL,
            radius::CONTROL,
            radius::SMALL,
            radius::TAG,
            radius::BADGE,
            radius::METER,
        ] {
            assert!(
                radius_scale.contains(&f32::from(value)),
                "{value:?} off the radii"
            );
        }

        let space_scale = [4., 6., 8., 10., 12., 14., 18., 22., 26., 34.];
        for value in [
            space::XS,
            space::SM,
            space::MD,
            space::LG,
            space::XL,
            space::XXL,
            space::SECTION,
            space::DIALOG,
            space::BLOCK,
            space::PAGE,
        ] {
            assert!(
                space_scale.contains(&f32::from(value)),
                "{value:?} off the spacing"
            );
        }

        let row_scale = [44., 40., 34., 30., 28., 25.];
        for value in [
            row::TOOLBAR,
            row::TITLE_BAR,
            row::HEADER,
            row::SEARCH,
            row::PROJECT,
            row::SESSION,
        ] {
            assert!(
                row_scale.contains(&f32::from(value)),
                "{value:?} off the row scale"
            );
        }

        // Motion is a closed list too, and the third animation is the reason it
        // is asserted rather than trusted: a duration is the one kind of token a
        // call site can invent without anybody seeing a wrong pixel.
        let motion_scale = [120u64, 1_400, 480];
        for value in [motion::CARET, motion::SKELETON_PULSE, motion::ANNOUNCE] {
            assert!(
                motion_scale.contains(&(value.as_millis() as u64)),
                "{value:?} off the motion scale"
            );
        }
        // Two curves, and they are a pair: the next animation either reuses
        // this one or argues for its own.
        let curves = [motion::ANNOUNCE_RISE, motion::ANNOUNCE_SETTLE];
        assert_eq!(curves.len(), 2, "the curve list is closed at one pair");
        assert_ne!(
            motion::ANNOUNCE_RISE,
            motion::ANNOUNCE_SETTLE,
            "a pair whose halves are equal is one curve named twice"
        );
    }

    /// The two curves are the design's own, and a solver is easy to get
    /// subtly wrong - so the endpoints and the shape are asserted rather than
    /// eyeballed on screen, where a 5px dot hides everything.
    #[test]
    fn the_announcement_curves_run_from_rest_to_rest() {
        for curve in [motion::ANNOUNCE_RISE, motion::ANNOUNCE_SETTLE] {
            assert!(curve.ease(0.0).abs() < 1e-4);
            assert!((curve.ease(1.0) - 1.0).abs() < 1e-4);
            // Monotone: an announcement that backs up mid-flight would read as
            // two beats, which is the thing 480 ms exists to avoid.
            let mut previous = 0.0;
            for step in 1..=100 {
                let value = curve.ease(step as f32 / 100.0);
                assert!(value >= previous - 1e-4, "{curve:?} went backwards");
                previous = value;
            }
        }
        // Both are ease-out shapes: most of the distance is covered early,
        // which is what makes the rise read as caused rather than as a drift.
        assert!(motion::ANNOUNCE_RISE.ease(0.25) > 0.75);
        assert!(motion::ANNOUNCE_SETTLE.ease(0.25) > 0.5);
    }

    /// Each scale is strictly descending, which is what lets a call site pick a
    /// neighbour when a role turns out to be one step off.
    #[test]
    fn each_scale_is_ordered() {
        assert!(text::DISPLAY > text::HEADING);
        assert!(text::HEADING > text::TITLE);
        assert!(text::TITLE > text::BODY);
        assert!(text::BODY > text::ROW);
        assert!(text::ROW > text::CONTROL);
        assert!(text::CONTROL > text::CAPTION);

        assert!(mono::BODY > mono::INLINE);
        assert!(mono::INLINE > mono::ROW);
        assert!(mono::ROW > mono::PATH);
        assert!(mono::PATH > mono::LABEL);
        assert!(mono::LABEL > mono::HINT);

        assert!(row::TOOLBAR > row::TITLE_BAR);
        assert!(row::TITLE_BAR > row::HEADER);
        assert!(row::HEADER > row::SEARCH);
        assert!(row::SEARCH > row::PROJECT);
        assert!(row::PROJECT > row::SESSION);
    }
}
