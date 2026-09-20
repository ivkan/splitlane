//! Multi-worktree diff viewer.
//!
//! This module hosts the `DiffView` GPUI entity and its tab plumbing; the diff
//! engine, the side-by-side render, and the N-column live view with its base
//! selector fill `DiffView` with real hunk data on top of this host.
//!
//! `DiffView` is the exact structural analog of `file_view::FileView`: an
//! `Entity` implementing `Render + Focusable`, hosted in a pane via the new
//! `TabContent::Diff` variant. It is ephemeral - never persisted to
//! `session.json` (like markdown tabs, dropped by `layout/serde.rs`).

mod align;
mod arrange;
mod element;
mod engine;
mod extract;
mod git;
mod highlighter;
mod hit_test;
mod hscroll;
mod multi_view;
mod review_terminal;
mod rows;
mod scope;
mod scope_header;
mod syntax;
mod view;
mod worddiff;

// Only the host view + its seed type are consumed outside this module
// (`pane::TabContent::Diff`, `event_handlers::open_multi_diff_for_repo`). The
// engine / git / rows types stay crate-internal, reached via `super::` paths.
// The read-only file surface colours its lines with the diff's own engine, so
// one file gets the same colours in both places and neither owns a second
// grammar table. Deliberately `highlight_lines` and not the donor's incremental
// `CodeHighlighter`: that one keeps a tree alive across keystrokes, which is
// the right shape for an editor and unnecessary for a file parsed once.
pub use git::{FileChange, list_repo_worktrees};
pub(crate) use highlighter::highlight_lines;
pub use multi_view::MultiRepoDiffView;
pub use scope::{DiffScope, RepoGroup};
pub(crate) use syntax::DiffSyntax as FileSyntax;
pub use view::{
    DiffView, DiffViewEvent, DiffWorktree, FileEntry, FileListState, aggregate_file_lists,
};

/// A `DiffView` for whatever repository `cwd` sits in, or `None` when it sits
/// in none.
///
/// Restore needs this: a `diff` tab in a saved layout carries no state of its
/// own - the diff is recomputed from git - so all it has to be rebuilt from is
/// the container's directory, and the container is not built yet when its
/// panes are. The repo is resolved exactly the way `Workspace::build` resolves
/// it, from the same three helpers, so the two answers cannot drift.
pub fn diff_view_for_cwd(
    cwd: &std::path::Path,
    workspace_id: u64,
    cx: &mut gpui::App,
) -> Option<gpui::Entity<DiffView>> {
    let cwd_str = cwd.to_string_lossy().into_owned();
    let git_dir = crate::workspace::git::find_git_dir(&cwd_str)?;
    let (repo_root, is_worktree) = crate::workspace::git::resolve_repo_root(&git_dir);
    let repo_root = repo_root?;
    let (branch, _) = crate::workspace::git::parse_head(&git_dir);
    let worktree_root = crate::workspace::git::resolve_worktree_root(
        &cwd_str,
        Some(git_dir.as_path()),
        Some(repo_root.as_path()),
        is_worktree,
    );
    let worktrees = vec![DiffWorktree {
        path: worktree_root,
        branch,
        workspace_id: Some(workspace_id),
    }];
    use gpui::AppContext;
    Some(cx.new(|cx| DiffView::new(repo_root, worktrees, cx)))
}
