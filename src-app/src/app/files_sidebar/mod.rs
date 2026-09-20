//! Docked Files right sidebar.
//!
//! Mirrors the agent-sessions sidebar (`sessions_sidebar.rs`): a
//! `flex_shrink_0` child of the root `flex_row`, toggled by the tab-bar Files
//! button via `PaneEvent::ToggleFilesSidebar`, mutually exclusive with the
//! sessions sidebar (one right column). Renders a lazily-expanded,
//! folders-first tree of the active target's `cwd` (the surface on screen).
//! Markdown rows are full-color + click-to-open into the active pane (the WCAG
//! 2.5.7 single-pointer alternative to dragging a row onto a pane); every
//! other file is greyed and inert; gitignored/hidden entries are filtered out before rendering.
//!
//! This module holds the state mutations (open/close, re-root, expand/collapse,
//! open-markdown) + the container render; the header/body/row rendering lives
//! in `view.rs`, and the pure tree model + fs helpers in `files_tree.rs`.

mod context_menu;
mod keyboard;
mod resize;

pub(crate) use resize::clamp_files_width;
mod row;
mod view;
mod watch;

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Pixels, Styled, Window,
    div, prelude::*, px,
};

use crate::app::files_tree::{self, FilesTreeState};
use crate::{SplitlaneApp, ToggleFilesSidebar};

/// The width the panel opens at, and the range the user may drag it to.
///
/// The design's 258 / 210-420, dragged from the panel's LEFT edge - the inner
/// one, the way the rail is dragged from its right. It used to be a fixed 300
/// with the resize deferred as out of scope; the design asks for the
/// same treatment the rail already got, so it is the same treatment.
pub(crate) const FILES_SIDEBAR_WIDTH: f32 = 258.;
pub(crate) const FILES_WIDTH_MIN: f32 = 210.;
pub(crate) const FILES_WIDTH_MAX: f32 = 420.;
/// The hit area of that edge. Five pixels, as on the rail: wider than the line
/// it drags, because a 1px target is not a target.
pub(crate) const FILES_RESIZE_HIT: f32 = 5.;
pub(super) const ROW_HEIGHT: Pixels = crate::app::constants::LIST_ROW_HEIGHT;
/// Per-depth indentation added to the row's left padding.
///
/// The design's 22 - a step of the spacing scale, and wide enough that a
/// nesting level reads at a glance in a panel this narrow. It was 12, which is
/// the step BETWEEN two adjacent scale values and made two levels look like
/// one.
pub(super) const INDENT_STEP: f32 = 22.;
/// Extra opacity knock-down for gitignored / hidden rows (the second tier).
pub(super) const DIMMED_OPACITY: f32 = 0.55;

impl SplitlaneApp {
    /// Toggle the Files sidebar. Opening resolves the active target's `cwd` to
    /// the tree root (the surface on screen), reads + auto-expands it, and closes the sessions
    /// sidebar (mutual exclusion). Re-clicking closes and releases the tree.
    pub(crate) fn handle_toggle_files_sidebar(
        &mut self,
        _: &ToggleFilesSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_files_sidebar(cx);
        if self.files_sidebar_open {
            self.files_focus.focus(window, cx);
        }
    }

    pub(crate) fn toggle_files_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.files_sidebar_open {
            self.close_files_sidebar(cx);
            return;
        }
        let Some(root) = self.files_tree_root(cx) else {
            return;
        };
        // Restore this directory's expansion (held on the Workspace,
        // so it survives a previous close within the session and a restart).
        let persisted = self.persisted_files_expansion(&root);

        // Mutual exclusion: only one right column is ever visible.
        if self.agent_sessions.sessions_sidebar_open
            || self.agent_sessions.sessions_sidebar_animation.is_some()
        {
            self.close_sessions_sidebar_immediate(cx);
        }
        // Floating dropdowns would paint over the docked panel.
        self.dismiss_transient_surfaces();

        self.set_files_sidebar_open(true, cx);
        self.files_tree_scroll = gpui::ScrollHandle::new();
        self.files_selected = 0;
        // Hydrate the tree + install non-recursive watches OFF the
        // render thread. A root shell paints this frame; `sync_files_expansion`
        // runs (and reconciles stale persisted paths back into `session.json`)
        // once hydration lands.
        self.spawn_files_hydration(root, persisted, cx);
    }

    /// Close the sidebar and release the per-open tree cache + watcher. The
    /// per-workspace expansion lives on the `Workspace`, so it is NOT reset
    /// here - reopening restores it.
    pub(crate) fn close_files_sidebar(&mut self, cx: &mut Context<Self>) {
        // Drop the watch + its channel while closed.
        self.files_watcher = None;
        self.files_event_rx = None;
        // Close any open row context menu so it can't outlive the tree.
        self.files_menu_open = None;
        self.set_files_sidebar_open(false, cx);
    }

    fn files_sidebar_width_at(&self, now: std::time::Instant) -> f32 {
        if let Some(animation) = self.files_sidebar_animation {
            animation.width_at(now)
        } else if self.files_sidebar_open {
            self.files_width
        } else {
            0.
        }
    }

    pub(crate) fn rendered_files_sidebar_width(&mut self, window: &mut Window) -> f32 {
        let now = std::time::Instant::now();
        if let Some(animation) = self.files_sidebar_animation {
            if animation.is_finished(now) {
                self.files_sidebar_animation = None;
                if !self.files_sidebar_open {
                    self.clear_files_sidebar_state();
                }
                animation.to_width
            } else {
                window.request_animation_frame();
                animation.width_at(now)
            }
        } else if self.files_sidebar_open {
            self.files_width
        } else {
            0.
        }
    }

    fn set_files_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let from_width = self.files_sidebar_width_at(now);
        self.files_sidebar_open = open;
        let to_width = if open { self.files_width } else { 0. };

        self.files_sidebar_animation =
            if (from_width - to_width).abs() > crate::PRIMARY_SIDEBAR_MIN_ANIMATION_DELTA {
                Some(crate::SidebarWidthAnimation {
                    from_width,
                    to_width,
                    started_at: now,
                })
            } else {
                None
            };

        if !open && self.files_sidebar_animation.is_none() {
            self.clear_files_sidebar_state();
        }
        cx.notify();
    }

    fn clear_files_sidebar_state(&mut self) {
        self.files_tree = FilesTreeState::default();
        self.files_watcher = None;
        self.files_event_rx = None;
        self.files_menu_open = None;
        self.files_selected = 0;
        self.files_git_marks.clear();
    }

    /// Re-root the tree on the active target's `cwd` when it changed while the
    /// sidebar is open (workspace switch, mode switch and Agents thread
    /// selection - the tree follows the surface on screen). No-op when closed or
    /// when the root is unchanged. Restores that directory's expansion and
    /// re-targets the watcher.
    pub(crate) fn reroot_files_tree(&mut self, cx: &mut Context<Self>) {
        if !self.files_sidebar_open {
            return;
        }
        let Some(root) = self.files_tree_root(cx) else {
            return;
        };
        if self.files_tree.root == root {
            return;
        }
        let persisted = self.persisted_files_expansion(&root);
        // Re-root off the render thread.
        self.spawn_files_hydration(root, persisted, cx);
    }

    /// The directory the Files tree belongs to - the cwd of whatever the
    /// window is showing, not of the CLI workspace list.
    ///
    /// The sidebar is not gated by mode, so it opens over Agents and Diff too.
    /// Reading the root from `workspaces[active_idx]` there showed a foreign
    /// directory - or, with no workspaces open at all, refused to open with no
    /// explanation. Every mode has an owner with a cwd: the active CLI
    /// workspace, the selected thread (its project's directory - the two are
    /// one-to-one), or the repo the diff is reviewing.
    ///
    /// This is separate from `refuse_markdown_without_a_pane`: that guard is
    /// about where a markdown *tab* can live, this is about what the tree is
    /// rooted at.
    fn files_tree_owner(&self, cx: &gpui::App) -> TreeOwner {
        let Some(root) = self
            .workspaces
            .get(self.active_idx)
            .and_then(|ws| ws.root.as_ref())
        else {
            return TreeOwner::Panes;
        };
        let Some(pane) = self
            .focused_pane_now
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .filter(|pane| root.contains_leaf(pane))
            .or_else(|| root.first_leaf())
        else {
            return TreeOwner::Panes;
        };
        let pane = pane.read(cx);
        match pane.tabs.get(pane.selected_idx) {
            Some(crate::pane::TabContent::Diff(_)) => TreeOwner::Diff,
            Some(crate::pane::TabContent::Terminal(view))
                if view.read(cx).agent_thread_id.is_some() =>
            {
                TreeOwner::AgentSurface
            }
            _ => TreeOwner::Panes,
        }
    }

    pub(crate) fn files_tree_root(&self, cx: &gpui::App) -> Option<PathBuf> {
        let active_workspace = self.workspaces.get(self.active_idx);
        // The tree follows the focused pane's surface, not a selection: an
        // agent pane roots it on that session's cwd, a diff pane on the repo
        // under review, and anything else on the container.
        let owner = self.files_tree_owner(cx);
        files_tree_root_for(
            owner,
            active_workspace.map(|ws| ws.cwd.as_str()),
            active_workspace.and_then(|ws| ws.repo_root.as_deref()),
            self.current_thread_view_target(cx)
                .and_then(|target| self.thread_for_target(target))
                .map(|thread| thread.cwd.as_str()),
        )
    }

    /// The persisted expansion set for `root`.
    ///
    /// Expansion belongs to the directory, not to the active index: keying it
    /// by cwd means an Agents thread and a CLI workspace on the same directory
    /// share one expansion, and a tree rooted somewhere no workspace covers
    /// simply starts collapsed instead of borrowing (and later overwriting)
    /// another directory's state.
    pub(super) fn persisted_files_expansion(&self, root: &Path) -> Vec<PathBuf> {
        self.workspaces
            .iter()
            .find(|ws| Path::new(&ws.cwd) == root)
            .map(|ws| ws.files_expanded.clone())
            .unwrap_or_default()
    }

    /// Expand or collapse a directory. First expand reads its listing (lazy,
    /// cached thereafter); when the live watcher is unavailable, every
    /// expand re-reads so manual navigation stays current without push updates.
    /// Reads are synchronous on the interaction (not the render path) per the
    /// deliberate "start synchronous" decision. Mirrors the expansion into the
    /// workspace + persists it.
    fn toggle_dir(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.files_tree.expanded.contains(path) {
            self.files_tree.expanded.remove(path);
            self.unwatch_files_dir(path);
        } else {
            self.files_tree.expanded.insert(path.to_path_buf());
            self.watch_files_dir(path);
            let stale =
                self.files_watcher.is_none() || !self.files_tree.children.contains_key(path);
            if stale {
                let listing = files_tree::read_dir_sorted(&self.files_tree.root, path);
                self.files_tree.children.insert(path.to_path_buf(), listing);
            }
        }
        self.sync_files_expansion();
        self.clamp_files_selection();
        self.save_session(cx);
        cx.notify();
    }

    /// True when the surface on screen cannot show a markdown tab, having told
    /// the user why.
    ///
    /// A markdown tab lives in a pane. The Files tree, however, opens over any
    /// surface - and an agent thread or a diff renders its own thing instead
    /// of the pane grid. Adding the tab anyway created it, focused it, and
    /// persisted it to `session.json` with nothing on screen and no message:
    /// the click read as "the app ignored me".
    pub(crate) fn refuse_markdown_without_a_pane(&mut self, cx: &mut Context<Self>) -> bool {
        if self.panes_surface_visible() {
            return false;
        }
        self.show_toast(
            "A file opens in a pane - select the project's panes to open this one.",
            cx,
        );
        true
    }

    /// Open a file by the targeting ladder, starting from the file pane.
    /// Reuses `FileView::open` unchanged; the sidebar stays open.
    ///
    /// Every file, not only markdown: what a file *is* - a rendered document,
    /// printed lines behind a number gutter, or something only the OS can open
    /// - is the surface's own answer.
    ///
    /// It used to aim at the pane markdown was opened in last
    /// (`files_surface_id`), falling back to the focused one. That was the
    /// ladder's first rung guessed from history rather than read off the
    /// screen: it aimed at a pane that had *once* held a document, whether or
    /// not it still did, and it had no answer at all for an empty pane or for
    /// the room to open a new one.
    pub(crate) fn open_file_in_active_pane(
        &mut self,
        path: PathBuf,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        if self.refuse_markdown_without_a_pane(cx) {
            return;
        }
        let ws_idx = self.active_idx;
        // The same file, already open: its pane takes focus and nothing is
        // re-read from disk.
        let wanted = path.clone();
        if self.reveal_tab_in_panes(
            ws_idx,
            move |tab, cx| match tab {
                crate::pane::TabContent::Markdown(view) => view.read(cx).path == wanted,
                _ => false,
            },
            cx,
        ) {
            return;
        }
        let markdown = cx.new(|cx| crate::file_view::FileView::open(path, cx));
        self.place_surface(
            ws_idx,
            crate::app::targeting::SurfaceKind::Markdown,
            crate::pane::TabContent::Markdown(markdown),
            cx,
        );
    }

    /// Render the docked Files sidebar. Only called when `files_sidebar_open`.
    pub(crate) fn render_files_sidebar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = crate::theme::ui_colors();
        div()
            .id("files-sidebar")
            .relative()
            .flex()
            .flex_col()
            .w(px(self.files_width))
            .flex_shrink_0()
            .h_full()
            .track_focus(&self.files_focus)
            .on_key_down(cx.listener(Self::handle_files_sidebar_key_down))
            // Match the app's other navigation rails: optional native material
            // on Windows, platform default on macOS, and a light/dark tint on Linux.
            .bg(crate::app::constants::cockpit_chrome_background(
                ui.chrome_for(window.is_window_active()),
                self.cached_config.cockpit_chrome_material_enabled(),
            ))
            .child(self.files_sidebar_header(ui, cx))
            .child(self.files_sidebar_body(ui, cx))
            .children(self.files_sidebar_footer(ui, cx))
            .child(self.render_files_resize_handle(cx))
            .into_any_element()
    }
}

/// Which surface the tree follows.
///
/// The root comes from whatever the content area is showing, so the
/// panel never roots on something the user is not looking at. An agent
/// surface follows the thread's cwd, the diff follows the repo under review,
/// and the panes follow their container. Pure, so the precedence is checked
/// without a live app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TreeOwner {
    Panes,
    AgentSurface,
    Diff,
}

fn files_tree_root_for(
    owner: TreeOwner,
    container_cwd: Option<&str>,
    container_repo_root: Option<&Path>,
    thread_cwd: Option<&str>,
) -> Option<PathBuf> {
    match owner {
        TreeOwner::Panes => container_cwd.map(PathBuf::from),
        TreeOwner::AgentSurface => thread_cwd.or(container_cwd).map(PathBuf::from),
        TreeOwner::Diff => container_repo_root
            .map(Path::to_path_buf)
            .or_else(|| container_cwd.map(PathBuf::from)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root comes from the surface on screen. The case that used to
    /// be wrong is the middle one - an agent thread showing the container's
    /// tree instead of its own cwd.
    #[test]
    fn tree_root_follows_the_visible_surface() {
        let container = Some("/w/cli");
        let repo = Path::new("/w");
        let thread = Some("/w/thread");

        assert_eq!(
            files_tree_root_for(TreeOwner::Panes, container, Some(repo), thread),
            Some(PathBuf::from("/w/cli")),
            "the panes follow their container, never the agent target"
        );
        assert_eq!(
            files_tree_root_for(TreeOwner::AgentSurface, container, Some(repo), thread),
            Some(PathBuf::from("/w/thread")),
            "an agent surface follows the thread's own cwd"
        );
        assert_eq!(
            files_tree_root_for(TreeOwner::AgentSurface, container, Some(repo), None),
            Some(PathBuf::from("/w/cli")),
            "the launcher has no thread yet: fall back rather than refuse to open"
        );
        assert_eq!(
            files_tree_root_for(TreeOwner::Diff, container, Some(repo), thread),
            Some(PathBuf::from("/w")),
            "the diff follows the repo under review"
        );
        assert_eq!(
            files_tree_root_for(TreeOwner::Diff, container, None, thread),
            Some(PathBuf::from("/w/cli")),
            "no repo resolved yet: the container cwd still gives a tree"
        );
        assert_eq!(
            files_tree_root_for(TreeOwner::AgentSurface, None, None, None),
            None,
            "nothing open anywhere is the only case with no root"
        );
    }
}
