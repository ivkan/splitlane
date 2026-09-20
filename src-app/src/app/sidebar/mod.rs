//! Sidebar rendering for `SplitlaneApp`: workspace rows, action buttons,
//! notification dropdown, and the context-menu row helpers (in the
//! [`context_menu`] submodule).
//!
//! Extracted from `main.rs` - pure code-motion, behaviour unchanged. Toast
//! utilities and sidebar-adjacent types (`WorkspaceContextMenu`,
//! `WorkspaceDrag`, `WorkspaceDragPreview`) remain in `main.rs` because they
//! cross module boundaries.

pub(crate) mod context_menu;

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    div,
};

use crate::ui_tokens as tok;
use crate::{SplitlaneApp, WorkspaceDrag, workspace::Workspace};

/// Memoized sibling-worktree ordering. Group labels stay hidden, but sibling
/// worktrees remain contiguous as before the visual redesign.
#[derive(Default)]
pub(crate) struct SidebarOrderCache {
    pub(crate) signature: Option<u64>,
    pub(crate) order: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceDropEdge {
    Before,
    After,
}

/// Collapse a `home`-rooted absolute path to a `~`-prefixed display string.
///
/// Uses [`std::path::Path::strip_prefix`] (component-boundary match,
/// OS-native separator) instead of a raw `str::starts_with` + byte slice. The
/// old form false-positived on a partial component (`/home/al` vs
/// `/home/alice`) and assumed `/` separators. Returns `cwd` verbatim when it
/// isn't under `home` (or `home` is empty), so a Windows casing mismatch
/// degrades to the full path rather than a wrong collapse.
pub(crate) fn collapse_home(cwd: &str, home: &str) -> String {
    if home.is_empty() {
        return cwd.to_string();
    }
    match std::path::Path::new(cwd).strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => cwd.to_string(),
    }
}

pub(crate) fn workspace_drop_edge(
    drag: &WorkspaceDrag,
    target_id: u64,
    target_idx: usize,
) -> Option<WorkspaceDropEdge> {
    if drag.id == target_id {
        None
    } else if drag.source_idx < target_idx {
        Some(WorkspaceDropEdge::After)
    } else {
        Some(WorkspaceDropEdge::Before)
    }
}

impl SplitlaneApp {
    fn begin_workspace_rename(&mut self, index: usize, cx: &gpui::App) {
        self.commit_rename(cx);
        if let Some(title) = self
            .workspaces
            .get(index)
            .map(|workspace| workspace.title.clone())
        {
            self.rename_text = title;
            self.renaming_idx = Some(index);
        }
    }

    pub(crate) fn sidebar_order_signature(workspaces: &[Workspace]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        workspaces.len().hash(&mut hasher);
        for workspace in workspaces {
            workspace.id.hash(&mut hasher);
            match &workspace.repo_root {
                Some(root) => root.hash(&mut hasher),
                None => 0u8.hash(&mut hasher),
            }
        }
        hasher.finish()
    }

    pub(crate) fn compute_display_order(workspaces: &[Workspace]) -> Vec<usize> {
        let mut repo_members: std::collections::HashMap<&std::path::Path, Vec<usize>> =
            std::collections::HashMap::new();
        for (index, workspace) in workspaces.iter().enumerate() {
            if let Some(root) = &workspace.repo_root {
                repo_members.entry(root.as_path()).or_default().push(index);
            }
        }

        let mut order = Vec::with_capacity(workspaces.len());
        let mut placed = vec![false; workspaces.len()];
        for (index, workspace) in workspaces.iter().enumerate() {
            if placed[index] {
                continue;
            }
            if let Some(root) = &workspace.repo_root
                && let Some(members) = repo_members.get(root.as_path())
                && members.len() >= 2
            {
                for &member in members {
                    order.push(member);
                    placed[member] = true;
                }
                continue;
            }
            order.push(index);
            placed[index] = true;
        }
        order
    }

    pub(crate) fn sidebar_list_wrapper(
        &self,
        list: gpui::Stateful<gpui::Div>,
        _cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        // The visible scroll bar was removed; wheel-scroll on the
        // inner `list` (driven by `overflow_y_scroll + track_scroll`)
        // is the only scrolling surface now. The wrapper still
        // exists so callers keep a stable insertion point if a
        // trailing affordance lands here later.
        div()
            .id("sidebar-list-wrapper")
            .relative()
            .flex_1()
            .flex()
            .flex_col()
            .min_h_0()
            .child(list)
    }
}

/// Lightweight tooltip body reused by sidebar affordances that just
/// need to show one short label. Mirrors the `WorkspaceCwdTooltip`
/// style minus the monospace font so prose reads naturally.
/// `pub(crate)`: the tab identity pill (pane.rs) reuses it rather
/// than duplicating a fourth one-label tooltip body.
pub(crate) struct SidebarTooltip {
    pub(crate) label: SharedString,
}

impl Render for SidebarTooltip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::active_theme();
        let ui = crate::theme::ui_colors();
        div()
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::SMALL)
            .bg(theme.title_bar_background)
            .border_1()
            .border_color(ui.border)
            .text_color(ui.text)
            .text_size(tok::text::CAPTION)
            .child(self.label.clone())
    }
}

/// Tooltip body for a workspace card. Surfaces the full cwd path so
/// it can stay off-screen on the card itself (the title is enough
/// signal at idle; the path is only relevant when the user needs to
/// distinguish two workspaces with similar titles or open a shell at
/// that exact location).
pub(crate) struct WorkspaceCwdTooltip {
    pub(crate) path: SharedString,
}

impl Render for WorkspaceCwdTooltip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::active_theme();
        let ui = crate::theme::ui_colors();
        div()
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::SMALL)
            .bg(theme.title_bar_background)
            .border_1()
            .border_color(ui.border)
            .text_color(ui.text)
            .text_size(tok::mono::ROW)
            .font_family(crate::ui_tokens::font::MONO)
            .child(self.path.clone())
    }
}
