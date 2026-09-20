//! The project toolbar: the 44px strip above the content area.
//!
//! It says which container is on screen - name, path, branch - and carries the
//! three controls the design puts there: Add pane, Orientation, Files. Nothing
//! in it is new behaviour. Add pane and Orientation act through the layout that
//! already exists, Files toggles the panel that already exists, and the branch
//! chip opens the picker the container's context menu already opened.
//!
//! **Neither control ever disappears**, and at the limit Add pane stays in
//! place and dims with its reason in a tooltip. The rule: a
//! control gated by a continuous quantity may not appear and disappear as that
//! quantity changes, because the quantity here is a width and a width is
//! dragged. The one exception is Orientation with a single pane, and it is not
//! the same case - there is nothing to orient, so the control has no state to
//! be in rather than a state it cannot reach.
//!
//! Under a system frame the toolbar also carries the waiting chip and the
//! palette hint at its right end, before Add pane - the title-bar row is not
//! drawn there, and D2 moves exactly those two and nothing else.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::SplitlaneApp;
use crate::layout::PaneForm;
use crate::theme::UiColors;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// A toolbar control's height. Physical: the design gives the row 44 and its
/// controls 26 and 24, which are sizes of objects rather than steps of the row
/// scale (44 / 40 / 34 / 30 / 28 / 25).
const CONTROL_HEIGHT: f32 = 26.;
/// One segment of the orientation control, and the box it holds - physical
/// sizes from the design, not steps of any scale. Three segments since the
/// grid arrived; the width is per segment and did not change with the count.
const ORIENTATION_HALF_W: f32 = 30.;
const ORIENTATION_GLYPH_W: f32 = 12.;
const ORIENTATION_GLYPH_H: f32 = 10.;
/// The branch chip is one step shorter than a button - it is a label with a
/// menu, not a control the eye lands on first.
const CHIP_HEIGHT: f32 = 24.;
/// The branch chip's leading dot.
const CHIP_DOT: f32 = 5.;

impl SplitlaneApp {
    /// The toolbar, or nothing when there is no container to name.
    pub(crate) fn render_project_toolbar(
        &self,
        show_waiting_cluster: bool,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ws_idx = self.active_idx;
        let workspace = self.workspaces.get(ws_idx)?;
        let title = SharedString::from(workspace.title.clone());
        let path = SharedString::from(crate::app::sidebar::collapse_home(
            &workspace.cwd,
            &self.home_dir,
        ));
        let branch = workspace.git_branch.clone();
        let has_repo = workspace.repo_root.is_some();
        // Orientation describes what is on screen, which is always the slot
        // tree: the toolbar is drawn only outside Settings, and Settings is the
        // one thing that is not a container's panes. A `showing_panes` flag
        // rode in here for the parked full-area surfaces - an agent or the diff
        // shown beside the tree rather than in it - and those were merged into
        // the tree in P5, so the flag had been constant `true` ever since.
        let panes = workspace.root.as_ref().map_or(0, |root| root.leaf_count());
        let form = workspace.root.as_ref().and_then(|root| root.root_form());

        let mut row = div()
            .flex_none()
            .h(tok::row::TOOLBAR)
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XL)
            .px(tok::space::XL)
            .bg(ui.chrome)
            .border_b_1()
            .border_color(ui.divider)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(tok::space::MD)
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .text_size(tok::text::BODY)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(ui.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::PATH)
                            .text_color(ui.faint)
                            .child(path),
                    ),
            )
            .child(self.branch_chip(ws_idx, &branch, has_repo, ui, cx))
            // The spacer. Everything after it is right-aligned.
            .child(div().flex_1().min_w_0());

        // D2: with no title-bar row, these two move here and nothing else does.
        if show_waiting_cluster {
            if let Some(chip) = crate::app::waiting::chip_state(self.activity()) {
                row = row.child(self.toolbar_waiting_chip(&chip, ui, cx));
            }
            row = row.child(toolbar_palette_hint(
                self.shortcut_for_action("open_command_palette")
                    .unwrap_or("Unassigned")
                    .to_string()
                    .into(),
                ui,
            ));
        }

        Some(
            row.child(self.split_button(ui, cx))
                .children(self.orientation_control(form, panes, ui, cx))
                .child(self.files_button(ui, cx))
                .into_any_element(),
        )
    }

    /// The branch, with a dot and a caret that opens the picker.
    ///
    /// A plain directory keeps the chip and loses the caret: there is nothing
    /// to switch to, and the design says so rather than hiding the chip - an
    /// absent chip reads as "the branch has not loaded yet".
    fn branch_chip(
        &self,
        ws_idx: usize,
        branch: &str,
        has_repo: bool,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label: SharedString = if !has_repo {
            "no repository".into()
        } else if branch.is_empty() {
            "detached".into()
        } else {
            branch.to_string().into()
        };
        let dot = if has_repo { ui.accent } else { ui.faint };
        let mut chip = div()
            .id("toolbar-branch")
            .flex_none()
            .h(px(CHIP_HEIGHT))
            .px(tok::space::MD)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::SM)
            .rounded(tok::radius::SMALL)
            .border_1()
            .border_color(ui.border)
            .font_family(tok::font::MONO)
            .text_size(tok::mono::PATH)
            .text_color(ui.text_secondary)
            .child(div().flex_none().size(px(CHIP_DOT)).rounded_full().bg(dot))
            .child(div().min_w_0().truncate().child(label));

        if has_repo {
            chip = chip
                .cursor_pointer()
                .tooltip(crate::ui_primitives::text_tooltip("Switch branch"))
                .child(
                    div()
                        .flex_none()
                        .text_color(ui.dim)
                        .child(SharedString::from("\u{25be}")),
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if let Some(position) = event.mouse_position() {
                        this.open_agents_branch_menu_for_project(ws_idx, position, window, cx);
                    }
                    cx.stop_propagation();
                }));
        }
        chip.into_any_element()
    }

    /// Add a pane. That is the whole of it.
    ///
    /// **It used to be `Split`, and `Split` was a mode without a settings row**.
    /// With one pane it added a second; with several it collapsed
    /// back to the focused one - so a control reading `Split` closed two live
    /// sessions on its third press, which is the only destructive action in
    /// this interface that had no name of its own. The generalisation was ours:
    /// the design wrote the reverse for the **empty** pane a split had just
    /// made ("nothing was opened … nothing to undo"), and the safety was in the
    /// word *empty*.
    ///
    /// So: one name, one action, up to the limit. Collapsing has its own row in
    /// the pane's `\u{22ef}` menu, where the target is named by where the menu was
    /// opened rather than implied. And the control has **no lit state** - a
    /// highlighted "do" button reads as "on", and a control that only adds has
    /// no "on" to mean. The orientation segment beside it already says whether
    /// a split exists, in the vocabulary of a two-state control.
    fn split_button(&self, ui: UiColors, cx: &mut Context<Self>) -> AnyElement {
        let hint = self.shortcut_for_action("add_pane").map(SharedString::from);
        // With no room the control **stays where it is**, dimmed, and says why.
        // The rule it follows: a control gated by a continuous
        // quantity may not appear and disappear as that quantity changes.
        // `MIN_PANE_FOR_SPLIT` is a width and width is dragged, so a button
        // that vanished at one window size and returned at another would make
        // the toolbar jump under the cursor of the hand not dragging - and take
        // away the only thing that could explain what had happened.
        //
        // **It asks the ladder's own question**, and it once did not:
        // it compared the count against the cap while the ladder measured
        // width, so on a 1332pt window the ladder refused a third pane at 304px
        // and this button created one. One question, three askers - here, the
        // ladder, and the drop strip. Now that the control only ever adds, that
        // question governs it in every state rather than only from one pane,
        // which is what an earlier fix for Stacked had to work around.
        let refusal = self.refuse_another_pane_now(cx);
        let full = refusal.is_some();
        let reason: Option<SharedString> = refusal.map(|r| r.sentence().into());
        toolbar_button_with_tooltip(
            "toolbar-split",
            "Add pane".into(),
            hint,
            // Never lit: see above. There is no state this control is "in".
            false,
            reason,
            if full { inert(ui) } else { ui },
            cx.listener(move |_, _: &ClickEvent, window, cx| {
                if full {
                    return;
                }
                window.dispatch_action(Box::new(crate::AddPane), cx);
                cx.stop_propagation();
            }),
        )
    }

    /// Which way the panes sit. The label states the current arrangement and
    /// the click flips it, through the layout presets that already exist.
    /// Orientation: a two-part segmented control, one half lit.
    ///
    /// It used to be a single button whose label was the current arrangement,
    /// and the design replaced it for a reason worth keeping written down: a
    /// button saying "stacked" reads as a status indicator, so the user looking
    /// at stacked panes finds nothing to press and no sign that another option
    /// exists. Both halves visible with one lit shows the state and the choice
    /// at once - the one segmented control in this toolbar, and the only place
    /// that earns the exception.
    /// `None` with one pane: there is nothing to orient, and the design removes
    /// the control rather than showing it disabled - a dead control still asks
    /// to be read.
    ///
    /// `None` and not an empty `div`, because the toolbar is a flex row with a
    /// gap: an element of zero width still takes a gap on each side, so the
    /// absent control left a double space between Add pane and Files.
    /// The grid gives it a **third** segment, and the segment is the only way
    /// into the grid: nothing here rearranges itself. A row of four is not
    /// drawable, and the honest handling of that is a refusal whose sentence
    /// names the form that would fit - one press instead of zero, in exchange
    /// for never having the screen rearranged under a running agent. Converting
    /// at the fourth pane would be the old `Split` defect one level up: a
    /// control that changes the arrangement under a person without being asked.
    ///
    /// The third glyph is a 2x2 of cells in the same box as the other two, so
    /// the control reads as three pictures of shapes rather than two shapes and
    /// a mode.
    fn orientation_control(
        &self,
        form: Option<PaneForm>,
        panes: usize,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if panes <= 1 {
            return None;
        }
        // A tree of two or more that is in none of the three forms cannot
        // happen - `flattened` is the only door in - but the control has to
        // draw something, and "row" is what a first split makes.
        let form = form.unwrap_or(PaneForm::Row);
        // The same rule again: gated by a width, so it dims in place rather
        // than disappearing, and the reason travels with it.
        let grid_fits = self.grid_fits_now();
        let grid_reason: Option<&'static str> =
            (!grid_fits).then_some(crate::app::targeting::GRID_REFUSAL);
        let control = div()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .overflow_hidden()
            // The rounding and the border sit on the group, not on the parts.
            .rounded(tok::radius::SMALL)
            .border_1()
            .border_color(ui.border)
            .bg(ui.overlay)
            .child(orientation_segment(
                "toolbar-orientation-rows",
                PaneForm::Row,
                form == PaneForm::Row,
                "Side by side",
                None,
                ui,
                cx.listener(move |_, _: &ClickEvent, window, cx| {
                    window.dispatch_action(Box::new(crate::LayoutEvenHorizontal), cx);
                    cx.stop_propagation();
                }),
            ))
            .child(orientation_segment(
                "toolbar-orientation-cols",
                PaneForm::Column,
                form == PaneForm::Column,
                "Stacked",
                None,
                ui,
                cx.listener(move |_, _: &ClickEvent, window, cx| {
                    window.dispatch_action(Box::new(crate::LayoutEvenVertical), cx);
                    cx.stop_propagation();
                }),
            ))
            .child(orientation_segment(
                "toolbar-orientation-grid",
                PaneForm::Grid,
                form == PaneForm::Grid,
                "Grid",
                grid_reason,
                if grid_fits { ui } else { inert(ui) },
                cx.listener(move |_, _: &ClickEvent, window, cx| {
                    if !grid_fits {
                        return;
                    }
                    window.dispatch_action(Box::new(crate::LayoutGrid), cx);
                    cx.stop_propagation();
                }),
            ));
        Some(control.into_any_element())
    }

    fn files_button(&self, ui: UiColors, cx: &mut Context<Self>) -> AnyElement {
        let hint = self
            .shortcut_for_action("toggle_files_sidebar")
            .map(SharedString::from);
        toolbar_button(
            "toolbar-files",
            "Files".into(),
            hint,
            self.files_sidebar_open,
            ui,
            cx.listener(move |_, _: &ClickEvent, window, cx| {
                window.dispatch_action(Box::new(crate::ToggleFilesSidebar), cx);
                cx.stop_propagation();
            }),
        )
    }

    /// The same chip the title bar draws, for the frame where the title bar is
    /// not drawn at all - through the same body, so the two frames share the
    /// rung, the dot's shape and the announcement, and differ only in the
    /// geometry that genuinely differs.
    ///
    /// See [`crate::app::waiting::chip_body`] for the scale, why the dot is on
    /// every rung, and why every value there is a role rather than a literal.
    fn toolbar_waiting_chip(
        &self,
        chip: &crate::app::waiting::ChipState,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let element = crate::app::waiting::chip_body(
            "toolbar-waiting-chip",
            CHIP_HEIGHT,
            CHIP_DOT,
            chip,
            self.chip_announcement(),
            ui,
        )
        .tooltip(crate::ui_primitives::text_tooltip(
            crate::app::waiting::chip_tooltip(chip),
        ))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.toggle_attention_queue(window, cx);
            cx.stop_propagation();
        }));
        if crate::app::waiting::chip_hovers(chip.rung) {
            element
                .animated_hover(move |style, delta| {
                    style.border_color(lerp_color(ui.border_strong, ui.border_hover, delta));
                })
                .into_any_element()
        } else {
            element.into_any_element()
        }
    }
}

/// A control whose text is the whole of it: label, optional chord, and the
/// accent treatment when whatever it toggles is on.
/// One half of the orientation control: a 30x26 hit area holding the 12x10
/// box whose 1px divider says which way the panes lie - vertical for side by
/// side, horizontal for stacked.
///
/// The divider rather than a thickened edge: the design draws Add pane's own
/// glyph as a box with a thick right border, and two neighbouring controls must
/// not carry the same shape for different claims. That glyph used to *be* state,
/// thicker while a split existed, and it was later fixed in place because it
/// describes the action rather than the situation. The shape is still spoken
/// for, so this one stays a divider.
/// One segment of the orientation control: a picture of the form, lit when the
/// panes are in it.
///
/// All three glyphs are drawn in the same box with the same 1px rules, so the
/// grid is a third *picture* rather than a differently-shaped control. The grid
/// is the row's rule plus the column's - which is what a 2x2's dividers are.
#[allow(clippy::too_many_arguments)]
fn orientation_segment(
    id: &'static str,
    form: PaneForm,
    active: bool,
    tooltip: &'static str,
    reason: Option<&'static str>,
    ui: UiColors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let glyph = if active { ui.accent } else { ui.dim };
    let box_el = div()
        .flex_none()
        .w(px(ORIENTATION_GLYPH_W))
        .h(px(ORIENTATION_GLYPH_H))
        .border_1()
        .border_color(glyph)
        .flex();
    let box_el = match form {
        PaneForm::Row => box_el
            .flex_row()
            .child(div().flex_1().border_r_1().border_color(glyph))
            .child(div().flex_1()),
        PaneForm::Column => box_el
            .flex_col()
            .child(div().flex_1().border_b_1().border_color(glyph))
            .child(div().flex_1()),
        // Two rows, each cut in two: the vertical rule is drawn twice with the
        // horizontal one between them, which is how a grid's shared divider
        // actually looks on screen.
        PaneForm::Grid => box_el
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .border_b_1()
                    .border_color(glyph)
                    .child(div().flex_1().border_r_1().border_color(glyph))
                    .child(div().flex_1()),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .child(div().flex_1().border_r_1().border_color(glyph))
                    .child(div().flex_1()),
            ),
    };
    div()
        .id(id)
        .flex_none()
        .w(px(ORIENTATION_HALF_W))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(if active {
            ui.accent_surface
        } else {
            gpui::transparent_black()
        })
        .tooltip(crate::ui_primitives::text_tooltip(
            reason.unwrap_or(tooltip),
        ))
        .on_click(on_click)
        .child(box_el)
        .into_any_element()
}

fn toolbar_button(
    id: &'static str,
    label: SharedString,
    hint: Option<SharedString>,
    active: bool,
    ui: UiColors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    toolbar_button_with_tooltip(id, label, hint, active, None, ui, on_click)
}

/// The same control, with a reason attached. Used where the control is inert
/// and the reason is the only thing worth saying about it.
#[allow(clippy::too_many_arguments)]
fn toolbar_button_with_tooltip(
    id: &'static str,
    label: SharedString,
    hint: Option<SharedString>,
    active: bool,
    tooltip: Option<SharedString>,
    ui: UiColors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let (border, background, text) = if active {
        (ui.accent_border, ui.accent_surface, ui.accent)
    } else {
        (ui.border, ui.overlay, ui.text_secondary)
    };
    let border_hover = if active {
        ui.accent_border
    } else {
        ui.border_hover
    };
    let mut button = div()
        .id(id)
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .px(tok::space::LG)
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::SM)
        .rounded(tok::radius::SMALL)
        .border_1()
        .bg(background)
        .text_size(tok::text::CONTROL)
        .text_color(text)
        .cursor_pointer()
        .child(label)
        .children(hint.map(|hint| {
            div()
                .flex_none()
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                .text_color(ui.dim)
                .child(hint)
        }))
        .on_click(on_click)
        .animated_hover(move |style, delta| {
            style.border_color(lerp_color(border, border_hover, delta));
        });
    if let Some(text) = tooltip {
        button = button.tooltip(crate::ui_primitives::text_tooltip(text));
    }
    button.into_any_element()
}

/// The `\u{2318}K` affordance, for the frame where the title bar does not carry
/// it. Same target, and the same rule about the label: it names the chord that
/// is really bound, not the one the design would like it to be.
fn toolbar_palette_hint(chord: SharedString, ui: UiColors) -> AnyElement {
    let resting = ui.dim;
    let hovered = ui.text_secondary;
    div()
        .id("toolbar-command-palette-trigger")
        .role(gpui::Role::Button)
        .aria_label("Open the command palette")
        .flex_none()
        .h(px(CONTROL_HEIGHT))
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
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(crate::OpenCommandPalette), cx);
        })
        .child(chord)
        .into_any_element()
}

/// The palette a disabled control draws with: one step down the text ramp and
/// no border of its own.
fn inert(ui: UiColors) -> UiColors {
    UiColors {
        text_secondary: ui.faint,
        border_hover: ui.border,
        ..ui
    }
}
