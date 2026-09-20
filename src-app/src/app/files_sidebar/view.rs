//! Files sidebar presentation: header + scrollable body. The per-row render
//! lives in `row.rs`; this file stays under the 250-line component budget.

use std::cell::Cell;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

struct FilesSidebarRenderTimeCanary {
    start: std::time::Instant,
    row_count: Cell<usize>,
}

impl FilesSidebarRenderTimeCanary {
    fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
            row_count: Cell::new(0),
        }
    }

    fn set_row_count(&self, row_count: usize) {
        self.row_count.set(row_count);
    }
}

impl Drop for FilesSidebarRenderTimeCanary {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        if elapsed > std::time::Duration::from_millis(16) {
            tracing::debug!(
                target: "splitlane_app::files_sidebar",
                "render_files_sidebar exceeded 16ms frame budget: {:.2}ms across {} visible rows",
                elapsed.as_secs_f64() * 1000.0,
                self.row_count.get()
            );
        }
    }
}

impl SplitlaneApp {
    /// The design's header: the section label `FILES` and, in the accent, how
    /// many files the repository has changed.
    ///
    /// The folder's own name used to be here. It is in the toolbar, one strip
    /// above and in the same column, so this row said the same word twice; the
    /// design spends it on the count instead - the one fact about the tree that
    /// is not visible in the tree.
    pub(super) fn files_sidebar_header(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover_background = crate::app::constants::sidebar_tab_hover_background();
        let changed = self.files_changed_count();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            // Quiet header - no divider (Codex: separation by spacing, not
            // borders), at the design's panel-header height.
            .h(tok::row::HEADER)
            .flex_none()
            .px(tok::space::XL)
            .child(crate::ui_primitives::section_eyebrow("FILES", ui))
            .children(changed.map(|count| {
                div()
                    .flex_none()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(ui.accent)
                    .child(SharedString::from(format!("{count} changed")))
            }))
            .child(div().flex_1().min_w_0())
            .child(
                div()
                    .id("files-sidebar-close")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(22.))
                    .rounded(tok::radius::TAG)
                    .text_size(tok::text::BODY)
                    .text_color(ui.muted)
                    .animated_hover(move |style, delta| {
                        style
                            .bg(lerp_color(
                                hover_background.opacity(0.0),
                                hover_background,
                                delta,
                            ))
                            .text_color(lerp_color(ui.muted, ui.text, delta));
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.close_files_sidebar(cx);
                        cx.stop_propagation();
                    }))
                    .child("×"),
            )
            .into_any_element()
    }

    /// The design's footer: the `UNCOMMITTED` label, the diffstat, and the
    /// button into the diff.
    ///
    /// Drawn only for a container that is a repository. A plain directory has
    /// nothing uncommitted and no review to open, and a Review button that
    /// opened an empty diff would be an affordance for nothing - the same call
    /// the status bar makes about the branch and the counts.
    pub(super) fn files_sidebar_footer(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let container = self.workspaces.get(self.active_idx)?;
        container.repo_root.as_ref()?;
        let stats = container.git_stats.clone();
        let diff = ui.diff_colors();
        let border_resting = ui.border;
        let border_hover = ui.border_hover;

        Some(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::MD)
                .h(tok::row::HEADER)
                .px(tok::space::XL)
                .border_t_1()
                .border_color(ui.divider)
                .child(crate::ui_primitives::section_eyebrow("UNCOMMITTED", ui))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(tok::space::SM)
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::LABEL)
                        .child(
                            div()
                                .text_color(diff.added)
                                .child(SharedString::from(format!("+{}", stats.insertions))),
                        )
                        .child(
                            div()
                                .text_color(diff.deleted)
                                .child(SharedString::from(format!("-{}", stats.deletions))),
                        ),
                )
                .child(div().flex_1().min_w_0())
                .child(
                    div()
                        .id("files-sidebar-review")
                        .flex_none()
                        .px(tok::space::MD)
                        .py(px(3.))
                        .rounded(tok::radius::SMALL)
                        .border_1()
                        .border_color(border_resting)
                        .text_size(tok::text::CONTROL)
                        .text_color(ui.text_secondary)
                        .cursor_pointer()
                        .animated_hover(move |style, delta| {
                            style.border_color(lerp_color(border_resting, border_hover, delta));
                        })
                        .on_click(cx.listener(|_this, _: &ClickEvent, window, cx| {
                            // The container's `Changes` surface - the same one
                            // the rail row of that name selects.
                            window.dispatch_action(Box::new(crate::OpenDiffView), cx);
                            cx.stop_propagation();
                        }))
                        .child("Review"),
                )
                .into_any_element(),
        )
    }

    /// How many files the active container's repository has changed, or `None`
    /// for a plain directory. `git_stats` already carries it - the same number
    /// the rail's meta line and the status bar read.
    fn files_changed_count(&self) -> Option<usize> {
        let container = self.workspaces.get(self.active_idx)?;
        container.repo_root.as_ref()?;
        Some(container.git_stats.files_changed)
    }

    pub(super) fn files_sidebar_body(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let canary = FilesSidebarRenderTimeCanary::new();
        let rows = self.files_visible_rows();
        canary.set_row_count(rows.len());

        if rows.is_empty() {
            let message = if self.files_tree.root_listing_ready() {
                "This folder is empty."
            } else {
                "Loading files..."
            };
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .p(tok::space::XL)
                .child(
                    div()
                        .text_size(tok::text::ROW)
                        .text_color(ui.muted)
                        .child(message),
                )
                .into_any_element();
        }

        // The rows go into a column that does not shrink, and that column is
        // the scroller's only child: a scroller's items have no automatic
        // minimum height, so rows placed straight into it gave up height to
        // fit a long tree instead of overflowing and scrolling.
        let mut column = div().flex_none().w_full().flex().flex_col();
        let selected = self.files_selected.min(rows.len().saturating_sub(1));
        for (idx, row) in rows.iter().copied().enumerate() {
            column = column.child(self.files_row(row, idx == selected, ui, cx));
        }
        div()
            .id("files-sidebar-body")
            .flex()
            .flex_col()
            .flex_1()
            .py(tok::space::XS)
            // Vertical scroll only - long names ellipsize, never scroll
            // horizontally.
            .overflow_x_hidden()
            .overflow_y_scroll()
            .track_scroll(&self.files_tree_scroll)
            .child(column)
            .into_any_element()
    }
}
