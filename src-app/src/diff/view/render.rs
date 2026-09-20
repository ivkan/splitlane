//! Render-oriented helpers shared by the DiffView host and its column model.

use super::*;
use crate::ui_primitives::{AnimatedHoverExt, BODY, LABEL, lerp_color, panel_empty_state};
use crate::ui_tokens as tok;
use crate::widgets::callout::{Callout, CalloutIcon, CalloutSeverity};

impl DiffView {
    fn render_arrange(&self, node: &Arrange, mode: ViewMode, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Arrange::Leaf(index) => self.render_pane(*index, mode, cx),
            Arrange::Split { axis, children } => {
                let ui = crate::theme::ui_colors();
                let row = *axis == Axis::Row;
                let mut container = div().size_full().flex().min_h_0().min_w_0();
                container = if row {
                    container.flex_row()
                } else {
                    container.flex_col()
                };
                for (index, child) in children.iter().enumerate() {
                    let mut cell = div().flex_1().min_h_0().min_w_0().flex();
                    if index > 0 {
                        cell = if row {
                            cell.border_l_1().border_color(ui.border)
                        } else {
                            cell.border_t_1().border_color(ui.border)
                        };
                    }
                    container = container.child(cell.child(self.render_arrange(child, mode, cx)));
                }
                container.into_any_element()
            }
        }
    }

    fn render_pane(&self, index: usize, mode: ViewMode, cx: &mut Context<Self>) -> AnyElement {
        let Some(column) = self.columns.get(index) else {
            return div().into_any_element();
        };
        let ui = crate::theme::ui_colors();
        let group_name = SharedString::from(format!("{}-pane-{index}", self.element_id));
        let region = self
            .drag_target
            .and_then(|(target, edge)| if target == index { edge } else { None });
        let mut overlay = div().absolute().size_full().flex();
        let (width, height) = match region {
            None => (relative(1.), relative(1.)),
            Some(DropEdge::Left) => {
                overlay = overlay.flex_row().justify_start();
                (relative(0.5), relative(1.))
            }
            Some(DropEdge::Right) => {
                overlay = overlay.flex_row().justify_end();
                (relative(0.5), relative(1.))
            }
            Some(DropEdge::Up) => {
                overlay = overlay.flex_col().justify_start();
                (relative(1.), relative(0.5))
            }
            Some(DropEdge::Down) => {
                overlay = overlay.flex_col().justify_end();
                (relative(1.), relative(0.5))
            }
        };
        let highlight = div()
            .w(width)
            .h(height)
            .bg(ui.accent.opacity(0.22))
            .border_2()
            // Theme accent instead of a hardcoded sky-blue hex.
            .border_color(ui.accent.opacity(0.75))
            .rounded(tok::radius::SMALL);
        let overlay = overlay
            .invisible()
            .group_drag_over::<DiffColumnDrag>(group_name.clone(), |style| style.visible())
            .on_drop(
                cx.listener(move |this, drag: &DiffColumnDrag, _window, cx| {
                    this.arrange_drop(drag.source_idx, index, cx);
                }),
            )
            .child(highlight);

        div()
            .id(SharedString::from(format!(
                "{}-panec-{index}",
                self.element_id
            )))
            .group(group_name)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_drag_move::<DiffColumnDrag>(cx.listener(
                move |this, event: &DragMoveEvent<DiffColumnDrag>, _window, cx| {
                    this.apply_drag_edge(index, event.bounds, event.event.position, cx);
                },
            ))
            .child(self.render_column(index, column, mode, cx))
            .child(overlay)
            .into_any_element()
    }

    fn apply_drag_edge(
        &mut self,
        index: usize,
        bounds: Bounds<Pixels>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let x = f32::from(position.x - bounds.origin.x);
        let y = f32::from(position.y - bounds.origin.y);
        let edge = compute_drop_edge(width, height, x, y, SPLIT_EDGE_BAND);
        let next = Some((index, edge));
        if self.drag_target != next {
            self.drag_target = next;
            cx.notify();
        }
    }

    fn arrange_drop(&mut self, source: usize, target: usize, cx: &mut Context<Self>) {
        let edge = self.drag_target.take().and_then(|(_, edge)| edge);
        if source == target {
            cx.notify();
            return;
        }
        let (axis, before) = match edge {
            Some(DropEdge::Left) => (Axis::Row, true),
            Some(DropEdge::Right) => (Axis::Row, false),
            Some(DropEdge::Up) => (Axis::Col, true),
            Some(DropEdge::Down) => (Axis::Col, false),
            None => (Axis::Row, true),
        };
        self.arrange.remove(source);
        self.arrange.split(target, axis, source, before);
        self.selected_column = source;
        self.scroll_driver = source;
        cx.notify();
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DiffViewEvent> for DiffView {}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        self.reload_visible_columns_if_theme_changed(cx);
        let root = div()
            .id(self.element_id.clone())
            .key_context("DiffView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &crate::CopyDiffHunk, window, cx| {
                this.copy_hovered_hunk(window, cx);
            }))
            // Keyboard-first review loop (DiffView && !Terminal).
            .on_action(cx.listener(|this, _: &crate::DiffNextHunk, window, cx| {
                this.goto_hunk(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::DiffPrevHunk, window, cx| {
                this.goto_hunk(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::DiffToggleView, _window, cx| {
                this.toggle_view_mode(cx);
            }))
            .on_action(cx.listener(|this, _: &crate::DiffToggleSync, _window, cx| {
                this.toggle_sync(cx);
            }))
            .on_action(cx.listener(|this, _: &crate::DiffDismiss, window, cx| {
                this.dismiss_overlays(window, cx);
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.h_scroll_drag.is_some() {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_horizontal_scrollbar(event.position.x, cx);
                    } else {
                        this.end_horizontal_scrollbar_drag(cx);
                    }
                } else if let Some((column_index, start_y, start_height)) = this.review_resizing
                    && let Some(column) = this.columns.get_mut(column_index)
                {
                    let delta_y = start_y - f32::from(event.position.y);
                    column.review_height =
                        (start_height + delta_y).clamp(REVIEW_MIN_HEIGHT, REVIEW_MAX_HEIGHT);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    this.end_horizontal_scrollbar_drag(cx);
                    if this.review_resizing.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.base)
            .text_color(ui.text);

        let mode = self.effective_mode(window);
        self.last_effective_mode = mode;
        self.ensure_visible_mode_loaded(mode, cx);
        // Consume the host-pushed scope breadcrumb (push-only contract: the
        // host re-pushes it every frame before this render runs). Rendered
        // even on the empty state - it carries the scope/project/branches
        // pickers, the only way OUT of an empty scope.
        let scope_slot = self.scope_slot.take();
        let files_slot = self.files_slot.take();

        if self.columns.is_empty() {
            return root
                .child(self.render_toolbar(mode, scope_slot, cx))
                .child(panel_empty_state(
                    ui,
                    Some("icons/git-branch.svg"),
                    Some("Nothing to compare".into()),
                    "This repository has no sibling worktrees to diff.",
                    false,
                ));
        }

        self.broadcast_scroll(mode);
        let visible: Vec<bool> = self.columns.iter().map(|column| column.visible).collect();
        self.arrange.reconcile(&visible);
        let body = self.render_arrange(&self.arrange, mode, cx);

        let root = root.child(self.render_toolbar(mode, scope_slot, cx));
        // `overflow_hidden` on the body band, not just `min_h_0`: `DiffElement`
        // reports its full content height, and with a footer under it a body
        // taller than the viewport painted straight over the footer. Clipping
        // the band is also what keeps the column's height definite, which is
        // what lets the scroll host cull (see `render_column`).
        let mut root = root
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .flex()
                    .children(files_slot)
                    .child(div().flex_1().min_w_0().flex().child(body)),
            )
            .child(self.render_footer(mode, cx));
        if let Some(menu) = &self.body_menu {
            root = root.child(self.render_body_menu(menu, ui, cx));
        }
        if let Some(flash) = &self.flash {
            root = root.child(self.render_flash(flash.clone(), ui));
        }
        root
    }
}

impl DiffView {
    /// Flip Unified ⇄ Split (the `u` binding). Mirrors the
    /// segmented control's inline writes.
    fn toggle_view_mode(&mut self, cx: &mut Context<Self>) {
        let next = match self.mode {
            ViewMode::Unified => ViewMode::Split,
            ViewMode::Split => ViewMode::Unified,
        };
        self.set_view_mode(next, cx);
    }

    fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        self.mode = mode;
        cx.notify();
    }

    /// `Esc` - close any open popover/menu and refocus the body so
    /// the keyboard loop continues. Order-independent.
    fn dismiss_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.base_picker_open = false;
        self.review_menu_open = None;
        self.body_menu = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn render_column(
        &self,
        idx: usize,
        col: &Column,
        mode: ViewMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let palette = palette(ui);

        // Review is offered per branch: live only when this column has changes,
        // highlighted while its own CLI-picker popover is open.
        let col_has_changes = Self::column_has_changes(col);
        let review_open = self.review_menu_open == Some(idx);

        let summary = match &col.state {
            ColumnState::Loading => "loading…".to_string(),
            ColumnState::Failed(_) => "error".to_string(),
            ColumnState::Loaded { file_count, .. } => match file_count {
                0 => "no changes".to_string(),
                1 => "1 file".to_string(),
                n => format!("{n} files"),
            },
        };

        // Selected column drives the sidebar file list + jump-to-file. Only
        // visually distinguished when there is more than one column.
        let selected = self.selected_column == idx && self.visible_count() > 1;
        // Per-column base toggle chip: shows what this column diffs against (the
        // shared base, or `HEAD~1` when overridden) and flips between the two on
        // click - one branch can show just its latest-commit delta while siblings
        // keep the whole-branch-vs-base view.
        let overridden = col.base_override.is_some();
        let eff_base = col
            .base_override
            .clone()
            .unwrap_or_else(|| self.base_ref.clone());
        let has_base = !eff_base.is_empty();
        let base_short: String = if eff_base.chars().count() > 12 {
            let s: String = eff_base.chars().take(11).collect();
            format!("{s}…")
        } else {
            eff_base
        };
        let base_chip_bg = if overridden {
            ui.accent.opacity(0.18)
        } else {
            ui.subtle.opacity(0.0)
        };
        let base_chip = div()
            .id(SharedString::from(format!("diff-col-base-{idx}")))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .px(tok::space::XS)
            .py(px(1.))
            .rounded(tok::radius::BADGE)
            .bg(base_chip_bg)
            .animated_hover_bg(base_chip_bg, ui.subtle)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.toggle_column_base(idx, cx);
            }))
            .child(
                gpui::svg()
                    .size(px(12.))
                    .flex_none()
                    .path("icons/git-pull-request.svg")
                    .text_color(if overridden { ui.accent } else { ui.muted }),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(LABEL)
                    .text_color(if overridden { ui.accent } else { ui.muted })
                    .child(base_short),
            );
        // Three-tier surface scale. The column header sits at a
        // distinct chrome tier from the body (`ui.base`) and the file cards
        // (`ui.surface`) on BOTH themes - `overlay` is darker than the body on
        // dark, but equal to it on light, so light falls back to `subtle`.
        let chrome_tier = if ui.base.l > 0.5 {
            ui.subtle
        } else {
            ui.overlay
        };
        // Grab handle for drag-to-rearrange (inc 5): the branch name is the drag
        // payload's ghost label. Click still selects (GPUI distinguishes click
        // from drag by a move threshold).
        let branch_drag = SharedString::from(col.branch.clone());
        let header = div()
            .id(SharedString::from(format!("diff-col-head-{idx}")))
            // Positioned ancestor for the Review CLI-picker popover below.
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .px(tok::space::MD)
            .py(tok::space::XS)
            // Selected = accent-tinted header + a 3px left accent
            // bar (below), replacing the ~invisible 1px bottom accent border.
            // The bottom border is now a neutral hierarchy divider only.
            .bg(if selected {
                ui.accent.opacity(0.08)
            } else {
                chrome_tier
            })
            .border_b_1()
            .border_color(ui.border)
            .when(selected, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(ui.accent),
                )
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_column(idx, cx);
                // Focus the body so the keyboard review loop is live.
                window.focus(&this.focus_handle, cx);
            }))
            .on_drag(
                DiffColumnDrag { source_idx: idx },
                move |_drag, _offset, _window, cx| {
                    cx.new(|_| TabDragPreview {
                        title: branch_drag.clone(),
                        icon: "icons/git-branch.svg".into(),
                    })
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if selected { ui.accent } else { ui.text })
                    .child(col.branch.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(LABEL)
                    .text_color(ui.muted)
                    .child(summary),
            )
            // Agent attribution badge between the file-count chip
            // and the base chip. Zero-width slot when no session matched.
            .children(self.render_attribution_badge(col, ui))
            .when(has_base, move |d| d.child(base_chip))
            // Review this branch: launch one or more CLIs against its diff. Sits
            // beside the terminal button; live only when the column has changes.
            // The signature "Review this branch" action is a
            // labeled pill (sparkles + text), not a bare eye icon - it reads as
            // the primary AI affordance instead of a mystery glyph.
            .when(col_has_changes, |d| {
                d.child(
                    crate::ui_primitives::toolbar_pill(
                        SharedString::from(format!("diff-col-review-{idx}")),
                        ui,
                        review_open,
                    )
                    .tooltip(crate::ui_primitives::text_tooltip(
                        "Review this branch with an AI agent",
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.toggle_review_menu(idx, cx);
                    }))
                    .child(
                        gpui::svg()
                            .size(px(12.))
                            .flex_none()
                            .path("icons/sparkles.svg")
                            .text_color(if review_open { ui.text } else { ui.muted }),
                    )
                    .child("Review"),
                )
            })
            // Open a plain terminal in this branch's worktree, embedded under the
            // diff.
            .child(
                div()
                    .id(SharedString::from(format!("diff-col-term-{idx}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.))
                    .rounded(tok::radius::BADGE)
                    .animated_hover_bg(ui.text.opacity(0.0), ui.text.opacity(0.12))
                    .tooltip(crate::ui_primitives::text_tooltip(
                        "Open a shell here to run git commands in this worktree",
                    ))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_terminal_for_column(idx, window, cx);
                    }))
                    .child(
                        gpui::svg()
                            .size(px(12.))
                            .flex_none()
                            .path("icons/terminal.svg")
                            .text_color(ui.muted),
                    ),
            )
            .child(
                div()
                    .id(SharedString::from(format!("diff-col-hide-{idx}")))
                    .flex_none()
                    .px(tok::space::XS)
                    .text_size(BODY)
                    .text_color(ui.muted)
                    .animated_hover(move |style, delta| {
                        style.text_color(lerp_color(ui.muted, ui.text, delta));
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        // Worktree scope: deselect the branch from the scope (the
                        // host drops it + rebuilds) so it's strictly shown-or-not.
                        // Other scopes keep the in-place hide.
                        if this.close_removes {
                            if let Some(col) = this.columns.get(idx) {
                                cx.emit(DiffViewEvent::CloseColumn {
                                    path: col.path.clone(),
                                });
                            }
                        } else {
                            this.hide_column(idx, cx);
                        }
                    }))
                    .child("×"),
            )
            // Per-branch Review CLI-picker popover, anchored under this header.
            .when(review_open, |d| {
                d.child(self.render_review_menu(idx, ui, cx))
            });

        let body: AnyElement = match &col.state {
            // Designed loading + failure states, not raw strings.
            ColumnState::Loading => crate::ui_primitives::panel_empty_state(
                ui,
                Some("icons/loader-circle.svg"),
                None,
                "Computing diff…",
                true,
            )
            .into_any_element(),
            ColumnState::Failed(e) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .p(tok::space::XXL)
                .child(
                    Callout::new(CalloutSeverity::Error, "Diff failed")
                        .icon(CalloutIcon::TriangleAlert)
                        .description(e.clone())
                        .render(),
                )
                .into_any_element(),
            ColumnState::Loaded { file_count, .. } if *file_count == 0 => {
                let b = col.base_override.as_deref().unwrap_or(&self.base_ref);
                // Edge case: a designed "Clean" state, not a
                // raw centered string.
                panel_empty_state(
                    ui,
                    Some("icons/check.svg"),
                    Some("Clean".into()),
                    format!("No changes vs {b}"),
                    false,
                )
                .into_any_element()
            }
            ColumnState::Loaded { .. } if !col.has_rows_for_mode(mode) => panel_empty_state(
                ui,
                Some("icons/loader-circle.svg"),
                None,
                format!("Preparing {} diff…", mode.label()),
                true,
            )
            .into_any_element(),
            ColumnState::Loaded { .. } => {
                // Custom direct-paint element hosted in an overflow-scroll div:
                // the element reports full content height; the div clips/scrolls
                // and supplies the viewport clip the element culls against. Renders
                // the collapse-filtered views (`disp_*`). The scroll-wheel listener
                // marks this column the sync driver; the click listener maps the
                // click Y to a row and toggles that file's collapse if it landed
                // on a file header.
                let body = match mode {
                    ViewMode::Split => DiffBody::Split {
                        rows: col.disp_split.clone(),
                        offsets: col.disp_split_offsets.clone(),
                        max_line_no: col.disp_split_max_no,
                        spans: col.disp_split_spans.clone(),
                        h_offsets: col.h_offsets.clone(),
                    },
                    ViewMode::Unified => DiffBody::Unified {
                        rows: col.disp_unified.clone(),
                        offsets: col.disp_unified_offsets.clone(),
                        max_line_no: col.disp_unified_max_no,
                        spans: col.disp_unified_spans.clone(),
                        h_offsets: col.h_offsets.clone(),
                    },
                };
                let mut element = div()
                    .id(SharedString::from(format!("diff-col-{idx}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&col.el_scroll)
                    .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, window, cx| {
                        this.scroll_driver = idx;
                        this.apply_horizontal_wheel(idx, ev, window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            let mode = this.effective_mode(window);
                            if this.handle_horizontal_scrollbar_mouse_down(
                                idx,
                                ev.position,
                                mode,
                                cx,
                            ) {
                                cx.stop_propagation();
                            }
                        }),
                    )
                    // cursor-exempt: the diff body is text, not a control.
                    // A click here picks the hunk under the pointer, which is
                    // the same reason the mouse-move handler below exists -
                    // and the note there says the rest of it: this must not
                    // turn changed lines into hover-driven controls. A pointer
                    // over a whole column of source would claim every line is
                    // pressable.
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        this.handle_body_click(idx, ev, window, cx);
                    }))
                    // Track the pointer for `Ctrl+Shift+C` (hunk under cursor)
                    // without turning changed lines into hover-driven controls.
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _window, _cx| {
                        this.last_body_pos = Some((idx, ev.position));
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            let mode = this.effective_mode(window);
                            this.open_body_menu(idx, ev.position, mode, cx);
                        }),
                    )
                    .child(
                        DiffElement::new(body, palette)
                            .with_current_hunk(Self::current_hunk_range(col, mode)),
                    );
                element.style().restrict_scroll_to_axis = Some(true);
                element.into_any_element()
            }
        };

        div()
            .flex_1()
            // `h_full` + `min_h_0`: pin the column to the (definite) height of the
            // horizontally-scrolling columns row. Without a definite height the
            // `overflow_y_scroll` host can't clip, so `DiffElement` (which reports
            // full content height) would paint every row instead of culling to the
            // viewport - the scroll lag. With it, only the ~viewport rows paint.
            .h_full()
            .min_h_0()
            // Panes shrink to share the split evenly (inc 5); the DiffElement
            // clips long lines per-pane, so a narrow pane shows fewer columns of
            // code rather than overflowing. Borders are drawn by the arrangement
            // walk between siblings, so the column itself draws none.
            .min_w_0()
            .flex()
            .flex_col()
            // Codex redesign: the column header only earns its row when there
            // are multiple columns to tell apart. Solo column: the branch is
            // already in the breadcrumb + sidebar; its Review/Terminal actions
            // live in the toolbar (see `render_toolbar`).
            .children((self.visible_count() > 1).then_some(header))
            .child(body)
            // Embedded review CLIs render UNDER the diff body, in the Diff
            // interface.
            .children(self.render_review_terminals(idx, col, ui, cx))
    }

    /// The single Diff-mode chrome row (Codex redesign): scope breadcrumb
    /// (host-pushed `scope_slot`) › base selector on the left; hunk nav +
    /// list actions + view-mode on the right. No own background and no
    /// border - it sits directly on the panel (`ui.base`), separation by
    /// spacing. The diffstat is gone from here: it lives ONCE, in the
    /// sidebar "Changes" header. In single-column scopes the per-column
    /// Review/Terminal buttons migrate here (the column header is hidden).
    /// The Review surface's header, per the design's "Diff review".
    ///
    /// Left: `±`, the name "Review", the badge "vs <branch>" (which is also the
    /// revision picker - one thing saying what the comparison is *and* letting
    /// it be changed, rather than a label beside a control that repeats it),
    /// and the status "N files · uncommitted". Right: the view chip and `⤢`.
    ///
    /// The controls between them - collapse-all, hidden columns, sync scroll,
    /// the per-branch Review and terminal actions, the attribution total - have
    /// no place in the design's header and no other home; they are grouped
    /// after the spacer rather than dropped. Recorded as a divergence.
    fn render_toolbar(
        &self,
        effective: ViewMode,
        scope_slot: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let hidden = self.columns.len() - self.visible_count();
        // Derived live (not a cached flag) so the chip can never disagree with the
        // real per-column collapse state.
        let all_collapsed = self.all_visible_collapsed();

        // Single-column scope: the column header row is not rendered, so its
        // Review / Terminal actions surface here instead.
        let solo_idx = (self.visible_count() == 1)
            .then(|| self.selected_or_first_visible())
            .flatten();

        // Pill control (icon + label). `active` paints the resting highlight
        // (open popover / toggle on).
        let control =
            |id: &'static str, active: bool| crate::ui_primitives::toolbar_pill(id, ui, active);
        let icon = |path: &'static str| {
            gpui::svg()
                .size(px(12.))
                .flex_none()
                .path(path)
                .text_color(ui.muted)
        };

        div()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .h(tok::row::HEADER)
            .flex_none()
            .px(tok::space::XL)
            .bg(ui.chrome)
            .border_b_1()
            .border_color(ui.divider)
            // --- left: what this surface is ---
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::SM)
                    // `±` - the design names the surface by a typographic mark,
                    // and Splitlane ships no asset for one.
                    .child(div().flex_none().text_size(tok::text::ROW).child("±"))
                    .child(
                        div()
                            .flex_none()
                            .text_size(tok::text::ROW)
                            .text_color(ui.text)
                            .child("Review"),
                    ),
            )
            // --- the comparison, which is also the picker for it ---
            .child(
                control("diff-base-chip", self.base_picker_open)
                    .tooltip(crate::ui_primitives::text_tooltip(
                        "Compare against another revision",
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_base_picker(window, cx);
                    }))
                    .child(
                        div()
                            .text_color(if self.base_ref.is_empty() {
                                ui.muted
                            } else {
                                ui.text_secondary
                            })
                            .child(self.comparison_label()),
                    )
                    .child(icon("icons/chevron-down.svg")),
            )
            .when(self.base_picker_open, |d| {
                d.child(deferred(self.render_base_popover(cx)).with_priority(10))
            })
            // --- and what it found ---
            .when_some(self.changed_files_status(), |d, status| {
                d.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::PATH)
                        .text_color(ui.muted)
                        .child(status),
                )
            })
            // --- the scope breadcrumb (host slot). The design has no scope
            // picker; this app has three scopes and this is the only way out
            // of an empty one. It is also the first thing to give up room: it
            // is the only part of this header the design does not draw, so a
            // narrow pane clips it rather than pushing the view chip and `⤢`
            // off the right edge. ---
            .when_some(scope_slot, |d, slot| {
                d.child(
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(slot),
                )
            })
            // --- spacer ---
            .child(div().flex_1().min_w_0())
            // --- the controls the design's header has no place for, in one
            // group that gives up room before the view chip and `⤢` do. They
            // have no other home, so they are kept; they are also the second
            // thing this header sacrifices, after the breadcrumb. ---
            .child(
                div()
                    .flex_shrink(1.)
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::MD)
                    // Aggregated estimated cost across attributed
                    // worktrees. Hidden when nothing is priced (no fabricated total).
                    .when_some(self.attribution_total(), |d, (total, n)| {
                        let cost = crate::pricing::format_cost(total);
                        d.child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(tok::space::XS)
                                .text_size(LABEL)
                                .text_color(ui.muted)
                                .child(
                                    gpui::svg()
                                        .size(px(12.))
                                        .flex_none()
                                        .path("icons/sparkles.svg")
                                        .text_color(ui.muted),
                                )
                                .child(if n == 1 {
                                    format!("{cost} est.")
                                } else {
                                    format!("{cost} est. · {n}")
                                }),
                        )
                    })
                    // --- single-column: per-branch Review / Terminal actions, migrated
                    // from the (hidden) column header. The Review popover anchors to
                    // its button's relative wrapper.
                    .when_some(solo_idx, |d, idx| {
                        let col_has_changes =
                            self.columns.get(idx).is_some_and(Self::column_has_changes);
                        let review_open = self.review_menu_open == Some(idx);
                        d.when(col_has_changes, |d| {
                            d.child(
                                div()
                                    .relative()
                                    .child(
                                        // Labeled Review pill (sparkles + text),
                                        // matching the per-column header action.
                                        crate::ui_primitives::toolbar_pill(
                                            "diff-toolbar-review",
                                            ui,
                                            review_open,
                                        )
                                        .tooltip(crate::ui_primitives::text_tooltip(
                                            "Review this branch with an AI agent",
                                        ))
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, _w, cx| {
                                                this.toggle_review_menu(idx, cx);
                                            },
                                        ))
                                        .child(
                                            gpui::svg()
                                                .size(px(12.))
                                                .flex_none()
                                                .path("icons/sparkles.svg")
                                                .text_color(if review_open {
                                                    ui.text
                                                } else {
                                                    ui.muted
                                                }),
                                        )
                                        .child("Review"),
                                    )
                                    .when(review_open, |d| {
                                        d.child(self.render_review_menu(idx, ui, cx))
                                    }),
                            )
                        })
                        .child(
                            crate::ui_primitives::icon_button_md(
                                "diff-toolbar-terminal",
                                "icons/terminal.svg",
                                ui.muted,
                                ui.text.opacity(0.12),
                            )
                            .tooltip(crate::ui_primitives::text_tooltip(
                                "Open a shell here to run git commands in this worktree",
                            ))
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.open_terminal_for_column(idx, window, cx);
                                },
                            )),
                        )
                    })
                    // --- list actions ---
                    .child(
                        control("diff-collapse-all", false)
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.toggle_collapse_all(cx);
                            }))
                            .text_color(ui.muted)
                            .child(icon(if all_collapsed {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron_up.svg"
                            }))
                            .child(if all_collapsed {
                                "Expand all"
                            } else {
                                "Collapse all"
                            }),
                    )
                    .when(hidden > 0, |d| {
                        d.child(
                            control("diff-show-hidden", false)
                                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                    this.show_all_columns(cx);
                                }))
                                .text_color(ui.muted)
                                .child(format!("{hidden} hidden")),
                        )
                    })
                    .when(self.visible_count() > 1, |d| {
                        d.child(
                            control("diff-sync-toggle", self.sync_scroll)
                                .on_click(
                                    cx.listener(|this, _: &ClickEvent, _w, cx| {
                                        this.toggle_sync(cx)
                                    }),
                                )
                                .tooltip(crate::ui_primitives::text_tooltip(if self.sync_scroll {
                                    "Columns scroll together (s)"
                                } else {
                                    "Columns scroll independently (s)"
                                }))
                                .child(icon("icons/link.svg")),
                        )
                    }),
            )
            // --- right: the view chip, then `⤢` ---
            .child(
                control("diff-view-chip", false)
                    .tooltip(crate::ui_primitives::text_tooltip(
                        "Switch the diff layout (u)",
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.toggle_view_mode(cx);
                    }))
                    .text_color(ui.text_secondary)
                    .child(icon(match effective {
                        ViewMode::Unified => "icons/diff-unified.svg",
                        ViewMode::Split => "icons/diff-split.svg",
                    }))
                    .child(match effective {
                        ViewMode::Unified => "unified",
                        ViewMode::Split => "side by side",
                    }),
            )
            .child(
                crate::ui_primitives::icon_button_md(
                    "diff-full-window",
                    "icons/arrows-diagonal.svg",
                    ui.muted,
                    ui.text.opacity(0.12),
                )
                .tooltip(crate::ui_primitives::text_tooltip(
                    "Give the review the whole window",
                ))
                .on_click(cx.listener(|_this, _: &ClickEvent, window, cx| {
                    window.dispatch_action(Box::new(crate::DiffFullWindow), cx);
                })),
            )
    }

    /// "vs <branch>", or "vs working tree" when no revision resolved - the
    /// design's badge, which says what the diff is *against* rather than
    /// naming a ref out of context.
    fn comparison_label(&self) -> String {
        if self.base_ref.is_empty() {
            "vs working tree".to_string()
        } else {
            format!("vs {}", self.base_ref)
        }
    }

    /// The design's "6 files · uncommitted". `None` while nothing has loaded -
    /// a "0 files" that means "not read yet" is worse than no status at all.
    fn changed_files_status(&self) -> Option<String> {
        let (_, files, _, _) = aggregate_file_lists(&self.column_file_lists());
        (files > 0).then(|| {
            let noun = if files == 1 { "file" } else { "files" };
            format!("{files} {noun} · uncommitted")
        })
    }

    /// The Review surface's footer: the keys that drive it, and where the
    /// reader is. Per the design's "] next hunk · [ prev · u unified / split ·
    /// s sync scroll" and "hunk 2 of 9".
    ///
    /// The hunk counter used to sit in the header beside a pair of chevrons.
    /// It reads better here, next to the keys that move it, and it leaves the
    /// header saying only what the surface is.
    fn render_footer(&self, effective: ViewMode, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let hunk_nav = self.hunk_position(effective);
        let nav_btn = |id: &'static str, icon_path: &'static str| {
            crate::ui_primitives::icon_button_sm(id, icon_path, ui.muted, ui.subtle)
        };
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .h(tok::row::PROJECT)
            .px(tok::space::XL)
            .bg(ui.chrome)
            .border_t_1()
            .border_color(ui.divider)
            .font_family(tok::font::MONO)
            .text_size(tok::mono::LABEL)
            .text_color(ui.dim)
            .child("] next hunk · [ prev · u unified / split · s sync scroll")
            .child(div().flex_1())
            .when_some(hunk_nav, |d, (total, current)| {
                let shown = current.clamp(1, total);
                d.child(
                    nav_btn("diff-hunk-prev", "icons/chevron_up.svg")
                        .tooltip(crate::ui_primitives::text_tooltip("Previous hunk ([)"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.goto_hunk(false, window, cx);
                        })),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(ui.muted)
                        .child(format!("hunk {shown} of {total}")),
                )
                .child(
                    nav_btn("diff-hunk-next", "icons/chevron-down.svg")
                        .tooltip(crate::ui_primitives::text_tooltip("Next hunk (])"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.goto_hunk(true, window, cx);
                        })),
                )
            })
    }

    /// Content-space `(top, bottom)` of the hunk this column's reader is on, for
    /// the accent stripe the element paints. The bottom is the next hunk's top,
    /// or the end of the content for the last one - `hunk_tops` states where
    /// each hunk begins and that is enough, because hunks are contiguous in the
    /// row list they index into.
    fn current_hunk_range(col: &Column, mode: ViewMode) -> Option<(f32, f32)> {
        let tops = col.hunk_tops(mode);
        if tops.is_empty() {
            return None;
        }
        let cur_y = f32::from(-col.el_scroll.offset().y).max(0.0);
        let pivot = cur_y + HUNK_JUMP_MARGIN;
        let index = tops
            .partition_point(|&t| t <= pivot + 4.0)
            .saturating_sub(1);
        let top = *tops.get(index)?;
        let bottom = tops
            .get(index + 1)
            .copied()
            .unwrap_or_else(|| col.content_height(mode));
        // A hunk also ends where its file does. Without this the bar runs
        // through the next file's header card and into its first context lines.
        let bottom = match col.next_file_header_top(mode, top) {
            Some(header) => bottom.min(header),
            None => bottom,
        };
        Some((top, bottom))
    }

    /// `(total hunks, current index by scroll position)` for the selected
    /// column, or `None` when it has none. Stateless - read from the live
    /// scroll offset so it tracks manual scrolling as well as the `]` / `[`
    /// jumps.
    fn hunk_position(&self, effective: ViewMode) -> Option<(usize, usize)> {
        self.selected_or_first_visible()
            .and_then(|i| self.columns.get(i))
            .map(|col| {
                let tops = col.hunk_tops(effective);
                let cur_y = f32::from(-col.el_scroll.offset().y).max(0.0);
                // Report the hunk parked at/above the viewport top by
                // `goto_hunk` (it lands a hunk's first line HUNK_JUMP_MARGIN px
                // below the top), NOT a cumulative count of every hunk scrolled
                // past. Pivoting on `cur_y + HUNK_JUMP_MARGIN` makes the counter
                // read exactly the hunk the nav last jumped to.
                let pivot = cur_y + HUNK_JUMP_MARGIN;
                let current = tops.partition_point(|&t| t <= pivot + 4.0);
                (tops.len(), current)
            })
            .filter(|(total, _)| *total > 0)
    }
}
