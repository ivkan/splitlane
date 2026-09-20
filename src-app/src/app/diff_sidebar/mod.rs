//! Left git panel for the Git Diff mode
//! ([`splitlane_config::schema::AppMode::Diff`]).
//!
//! The panel is the Zed-styled changed-files tree (NOT the workspace
//! list - workspace switching stays on `Ctrl+1-9` / CLI mode). A "Changes"
//! section header (collapse chevron + aggregate diffstat) tops a list of file rows
//! (status-colored letter + filename + dimmed directory + +/- counts), with
//! hover / selected states resolved from the curated `vc_*` theme slots.
//! In multi-branch scopes the data is read per-column via
//! `DiffView::column_file_lists` and rendered as one collapsible section per
//! branch; single-column scopes render a flat list. The row renderer lives in
//! `rows.rs` to keep this file under the 250-line cap.

use crate::SplitlaneApp;
use crate::app::diff_view_actions::DIFF_FILE_LIST_WIDTH;
use crate::diff::{FileEntry, FileListState, aggregate_file_lists};
use crate::theme::UiColors;
use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Pixels, SharedString, Styled, div, prelude::*, px,
};
use std::collections::BTreeMap;

mod header;
mod rows;

/// One node of the changed-files directory tree (tree mode). `subdirs` is
/// sorted (BTreeMap) so folders render alphabetically; `files` holds indices
/// into the per-column `visible` slice. Built per render - cheap for the
/// changed-file counts a single diff produces.
#[derive(Default)]
struct DirNode {
    subdirs: BTreeMap<String, DirNode>,
    files: Vec<usize>,
}

const REVIEW_SIDEBAR_ROW_MARGIN_X: f32 = 8.0;
const REVIEW_SIDEBAR_ROW_PADDING_X: f32 = 8.0;
const REVIEW_SIDEBAR_ROW_RADIUS: Pixels = tok::radius::PANEL;
const REVIEW_SIDEBAR_LIST_GAP: f32 = 4.0;

impl SplitlaneApp {
    /// Sidebar render branch for [`AppMode::Diff`](splitlane_config::schema::AppMode::Diff).
    /// Wired into the `main.rs` mode-dispatch `match`.
    /// The diff surface's own file-list column: the changed files, their
    /// filter, and the branch/revision picker. This was the Diff rail; it is
    /// a column of the surface now, so the app has one rail and the list
    /// stays with the diff it describes.
    pub(crate) fn render_diff_file_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ui = crate::theme::ui_colors();

        div()
            .relative()
            .w(px(DIFF_FILE_LIST_WIDTH))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(ui.border)
            .flex()
            .flex_col()
            .child(self.render_diff_files(ui, cx))
            .into_any_element()
    }

    /// The "Changes" section + the changed-files list (or a centered
    /// loading / error / empty / no-repo message).
    fn render_diff_files(&self, ui: UiColors, cx: &mut Context<Self>) -> AnyElement {
        let mounted = match self.diff_mode.diff_scope {
            crate::diff::DiffScope::MultiProject => self.diff_mode.multi_diff_view.is_some(),
            _ => self.diff_mode.diff_view.is_some(),
        };
        if !mounted {
            // Edge case: a designed no-repo state - an icon, a
            // title, and a one-line positioning hint. The "open a project"
            // affordance is the scope-header project picker in the breadcrumb.
            return crate::ui_primitives::panel_empty_state(
                ui,
                Some("icons/git-branch.svg"),
                Some("No Git repository".into()),
                "Open a project backed by a Git repo to review its changes here.",
                false,
            )
            .into_any_element();
        }

        // Per-branch (column) file lists + the active column, for the scope's
        // host. Multi-project reads the selected repo tab's columns; the other
        // scopes the single mounted DiffView. One section per column downstream.
        let (lists, selected_col): (
            Vec<(String, usize, std::path::PathBuf, FileListState)>,
            usize,
        ) = match self.diff_mode.diff_scope {
            crate::diff::DiffScope::MultiProject => self
                .diff_mode
                .multi_diff_view
                .as_ref()
                .map(|v| {
                    let v = v.read(cx);
                    (v.active_column_file_lists(cx), v.active_selected_column(cx))
                })
                .unwrap_or_default(),
            _ => self
                .diff_mode
                .diff_view
                .as_ref()
                .map(|v| {
                    let v = v.read(cx);
                    (v.column_file_lists(), v.selected_column())
                })
                .unwrap_or_default(),
        };

        if lists.is_empty() {
            // Mounted but no columns yet - the brief cold-mount / discovery window.
            // Edge case: animated loader + the active branch
            // being diffed, not a bare string.
            let branch = self
                .workspaces
                .get(self.active_idx)
                .map(|ws| ws.git_branch.clone())
                .filter(|b| !b.is_empty());
            let msg = if self.diff_mode.diff_discovering {
                "Discovering worktrees…".to_string()
            } else {
                match branch {
                    Some(b) => format!("Computing diff for {b}…"),
                    None => "Computing diff…".to_string(),
                }
            };
            return crate::ui_primitives::panel_empty_state(
                ui,
                Some("icons/loader-circle.svg"),
                None,
                msg,
                true,
            )
            .into_any_element();
        }

        let collapsed = self.diff_mode.diff_files_collapsed;

        // The filter field renders only when a file list actually
        // exists (≥ 1 changed file across the loaded columns) - filtering an
        // empty / still-loading list is meaningless chrome.
        let has_files = lists
            .iter()
            .any(|(_, _, _, st)| matches!(st, FileListState::Loaded(f) if !f.is_empty()));

        // Aggregate diffstat across every branch's files - an at-a-glance sense
        // of the total changeset, shown in the header even when collapsed. Same
        // source of truth as the diff view's aggregate strip (Loaded, non-empty
        // columns only) so the two totals can never drift apart.
        let (_, _, total_added, total_removed) = aggregate_file_lists(&lists);

        // Live path filter (case-insensitive substring) driven by the
        // always-visible filter field below the header. Empty ⇒ all match.
        let filter_lc = self
            .diff_mode
            .diff_file_filter
            .read(cx)
            .value()
            .to_lowercase();
        let filtering = !filter_lc.is_empty();

        let header = self.render_diff_files_header(ui, collapsed, total_added, total_removed, cx);

        // Always-on filter field (cursor-aware TextInput). Escape clears it; a
        // clear-(×) button appears once it has content. Shares the [`filter_pill`]
        // primitive with the Agents sidebar filter.
        let filter_field = crate::ui_primitives::filter_pill(
            "diff-files-filter",
            "diff-files-filter-clear",
            ui,
            self.diff_mode.diff_file_filter.clone(),
            filtering,
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.diff_mode.diff_file_filter.update(cx, |inp, cx| {
                    inp.clear(cx);
                });
            }),
        )
        .mx(tok::space::MD)
        .mt(tok::space::XS)
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
            if ev.keystroke.key.as_str() == "escape" {
                this.diff_mode.diff_file_filter.update(cx, |inp, cx| {
                    inp.clear(cx);
                });
                cx.stop_propagation();
            }
        }));

        // Single column ⇒ flat list (Project / one worktree); multiple ⇒ one
        // collapsible section per branch so all branches are visible at once.
        let body: Vec<AnyElement> = if collapsed {
            Vec::new()
        } else if lists.len() == 1 {
            match lists.first() {
                Some((_, col_idx, _, st)) => {
                    self.render_diff_file_rows(*col_idx, true, st, &filter_lc, ui, cx)
                }
                None => Vec::new(),
            }
        } else {
            lists
                .iter()
                .map(|(branch, col_idx, path, st)| {
                    self.render_diff_branch_section(
                        branch,
                        &path.to_string_lossy(),
                        *col_idx,
                        *col_idx == selected_col,
                        st,
                        &filter_lc,
                        ui,
                        cx,
                    )
                })
                .collect()
        };

        let mut container = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header);
        if !collapsed && has_files {
            container = container.child(filter_field);
        }
        container
            .child(
                div()
                    .id("diff-files-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(REVIEW_SIDEBAR_LIST_GAP))
                    .pt(px(REVIEW_SIDEBAR_LIST_GAP))
                    .pb(tok::space::MD)
                    .children(body),
            )
            .into_any_element()
    }

    /// File rows for ONE column (branch), filtered. `is_active` marks the column
    /// whose selection drives the body, so the selected-file highlight only
    /// lights up in the active branch's section (a filename present in several
    /// branches isn't highlighted everywhere). A loading / failed / empty column
    /// yields a single muted note row.
    fn render_diff_file_rows(
        &self,
        col_idx: usize,
        is_active: bool,
        state: &FileListState,
        filter_lc: &str,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let filtering = !filter_lc.is_empty();
        let note = |msg: String| {
            div()
                .mx(px(REVIEW_SIDEBAR_ROW_MARGIN_X))
                .px(px(REVIEW_SIDEBAR_ROW_PADDING_X))
                .py(tok::space::MD)
                .text_color(ui.muted)
                .text_size(crate::ui_primitives::BODY)
                .child(msg)
                .into_any_element()
        };
        match state {
            FileListState::Loading => vec![note("Computing diff…".into())],
            FileListState::Failed(e) => vec![note(e.clone())],
            FileListState::Loaded(files) => {
                let visible: Vec<&FileEntry> = files
                    .iter()
                    .filter(|e| !filtering || e.path.to_lowercase().contains(filter_lc))
                    .collect();
                if visible.is_empty() {
                    vec![note(
                        if filtering {
                            "No files match your filter"
                        } else {
                            "No changes"
                        }
                        .into(),
                    )]
                } else if self.diff_mode.diff_files_tree {
                    self.render_diff_file_tree(col_idx, is_active, &visible, ui, cx)
                } else {
                    visible
                        .iter()
                        .map(|&e| self.render_diff_file_row(e, col_idx, is_active, 0.0, ui, cx))
                        .collect()
                }
            }
        }
    }

    /// Tree-mode body for one column: build a nested directory tree from the
    /// filtered files and render it (folders sorted, single-child chains merged
    /// compact-folder style, per-directory collapse).
    fn render_diff_file_tree(
        &self,
        col_idx: usize,
        is_active: bool,
        visible: &[&FileEntry],
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut root = DirNode::default();
        for (i, e) in visible.iter().enumerate() {
            let mut node = &mut root;
            let mut segs = e.path.split('/').peekable();
            while let Some(seg) = segs.next() {
                if segs.peek().is_none() {
                    node.files.push(i); // last segment is the filename
                } else {
                    node = node.subdirs.entry(seg.to_string()).or_default();
                }
            }
        }
        let mut out = Vec::new();
        self.render_dir_node(&root, "", 0, col_idx, is_active, visible, ui, cx, &mut out);
        out
    }

    /// Recursively emit a directory node's folder rows (then its file rows).
    /// Compact-folder chaining collapses `a → b → c` (each a lone child) into a
    /// single `a/b/c` row so deep Rust paths don't waste horizontal space.
    #[allow(clippy::too_many_arguments)]
    fn render_dir_node(
        &self,
        node: &DirNode,
        prefix: &str,
        depth: usize,
        col_idx: usize,
        is_active: bool,
        visible: &[&FileEntry],
        ui: UiColors,
        cx: &mut Context<Self>,
        out: &mut Vec<AnyElement>,
    ) {
        const INDENT: f32 = 12.0;
        for (name, child) in &node.subdirs {
            let mut disp = name.clone();
            let mut full = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let mut cur = child;
            while cur.files.is_empty() && cur.subdirs.len() == 1 {
                let Some((sn, sc)) = cur.subdirs.iter().next() else {
                    break;
                };
                disp = format!("{disp}/{sn}");
                full = format!("{full}/{sn}");
                cur = sc;
            }
            let key = format!("{col_idx}\u{0}{full}");
            let collapsed = self.diff_mode.diff_collapsed_dirs.contains(&key);
            out.push(self.render_dir_header_row(col_idx, &disp, &full, collapsed, depth, ui, cx));
            if !collapsed {
                self.render_dir_node(
                    cur,
                    &full,
                    depth + 1,
                    col_idx,
                    is_active,
                    visible,
                    ui,
                    cx,
                    out,
                );
            }
        }
        for &fi in &node.files {
            out.push(self.render_diff_file_row(
                visible[fi],
                col_idx,
                is_active,
                depth as f32 * INDENT,
                ui,
                cx,
            ));
        }
    }

    /// One collapsible directory row in tree mode: chevron + folder glyph +
    /// (compacted) directory name, indented by `depth`. Collapse is keyed by
    /// `col_idx\0<full dir>` so the same directory in two branch sections folds
    /// independently.
    #[allow(clippy::too_many_arguments)]
    fn render_dir_header_row(
        &self,
        col_idx: usize,
        disp: &str,
        full: &str,
        collapsed: bool,
        depth: usize,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const INDENT: f32 = 12.0;
        let key = format!("{col_idx}\u{0}{full}");
        let hover_background = crate::app::constants::sidebar_tab_active_background();
        div()
            .id(SharedString::from(format!("diff-dir-{col_idx}-{full}")))
            .flex_none()
            // Folder rows and file rows interleave, so they share the one
            // list-row height every other list uses.
            .h(crate::app::constants::LIST_ROW_HEIGHT)
            .mx(px(REVIEW_SIDEBAR_ROW_MARGIN_X))
            .pl(px(REVIEW_SIDEBAR_ROW_PADDING_X + depth as f32 * INDENT))
            .pr(px(REVIEW_SIDEBAR_ROW_PADDING_X))
            .rounded(REVIEW_SIDEBAR_ROW_RADIUS)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .animated_hover_bg(hover_background.opacity(0.0), hover_background)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                if !this.diff_mode.diff_collapsed_dirs.remove(&key) {
                    this.diff_mode.diff_collapsed_dirs.insert(key.clone());
                }
                cx.notify();
            }))
            .child(
                gpui::svg()
                    .size(px(12.))
                    .flex_none()
                    .text_color(ui.muted)
                    .path(if collapsed {
                        "icons/chevron-right.svg"
                    } else {
                        "icons/chevron-down.svg"
                    }),
            )
            .child(
                gpui::svg()
                    .size(px(12.))
                    .flex_none()
                    .path(if collapsed {
                        "icons/folder.svg"
                    } else {
                        "icons/folder-open.svg"
                    })
                    .text_color(ui.muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::ui_primitives::BODY)
                    .text_color(ui.text)
                    .child(disp.to_string()),
            )
            .into_any_element()
    }

    /// One branch section in the multi-branch sidebar: a collapsible sub-header
    /// (branch name + file count + diffstat, active branch accented) over that
    /// branch's filtered file rows. Collapse is keyed by `collapse_key` (the
    /// worktree PATH, globally unique) - NOT the branch name, which collides
    /// across repos in Multi-project scope (every repo has a `main`).
    #[allow(clippy::too_many_arguments)]
    fn render_diff_branch_section(
        &self,
        branch: &str,
        collapse_key: &str,
        col_idx: usize,
        is_active: bool,
        state: &FileListState,
        filter_lc: &str,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section_collapsed = self
            .diff_mode
            .diff_collapsed_branches
            .contains(collapse_key);
        let (added, removed, count) = match state {
            FileListState::Loaded(files) => {
                let (a, r) = files
                    .iter()
                    .fold((0u32, 0u32), |(a, r), f| (a + f.added, r + f.removed));
                (a, r, files.len())
            }
            _ => (0, 0, 0),
        };
        let key_owned = collapse_key.to_string();
        let hover_background = crate::app::constants::sidebar_tab_active_background();
        let resting_background = if is_active {
            crate::app::constants::sidebar_tab_active_background()
        } else {
            hover_background.opacity(0.0)
        };
        let sub_header = div()
            .id(SharedString::from(format!("diff-branch-{col_idx}")))
            .flex_none()
            .h(crate::app::constants::LIST_ROW_HEIGHT)
            .mx(px(REVIEW_SIDEBAR_ROW_MARGIN_X))
            .px(px(REVIEW_SIDEBAR_ROW_PADDING_X))
            .rounded(REVIEW_SIDEBAR_ROW_RADIUS)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .bg(resting_background)
            .animated_hover_bg(resting_background, hover_background)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                if !this.diff_mode.diff_collapsed_branches.remove(&key_owned) {
                    this.diff_mode
                        .diff_collapsed_branches
                        .insert(key_owned.clone());
                }
                cx.notify();
            }))
            .child(
                gpui::svg()
                    .size(px(12.))
                    .flex_none()
                    .text_color(ui.muted)
                    .path(if section_collapsed {
                        "icons/chevron-right.svg"
                    } else {
                        "icons/chevron-down.svg"
                    }),
            )
            .child(
                gpui::svg()
                    .size(px(12.))
                    .flex_none()
                    .path("icons/git-branch.svg")
                    .text_color(if is_active { ui.accent } else { ui.muted }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::ui_primitives::BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if is_active { ui.accent } else { ui.text })
                    .child(branch.to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(crate::ui_primitives::LABEL)
                    .text_color(ui.muted)
                    .child(format!("{count}")),
            )
            .when(added > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(crate::ui_primitives::LABEL)
                        .text_color(ui.diff_colors().added)
                        .child(format!("+{added}")),
                )
            })
            .when(removed > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(crate::ui_primitives::LABEL)
                        .text_color(ui.diff_colors().deleted)
                        .child(format!("-{removed}")),
                )
            });

        let mut section = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(REVIEW_SIDEBAR_LIST_GAP))
            .child(sub_header);
        if !section_collapsed {
            section = section
                .children(self.render_diff_file_rows(col_idx, is_active, state, filter_lc, ui, cx));
        }
        section.into_any_element()
    }
}
