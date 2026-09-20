//! Lifecycle + main-area entry point for the dedicated Git Diff mode
//! ([`AppMode::Diff`]).
//!
//! This mirrors `agents_view_actions.rs`: the mode owns the full main
//! area plus its own left sidebar, entered via the CLI / Diff / Agents
//! segmented toggle (`render_mode_toggle`). The mode mounts the reused
//! `diff::DiffView` engine here, together with the scope selector.

use crate::diff::{DiffScope, DiffView, DiffViewEvent, DiffWorktree, RepoGroup};
use crate::{OpenDiffView, SplitlaneApp};
use gpui::{AppContext, Context, Entity, Focusable, Window};
use std::path::{Path, PathBuf};

/// Max retained single-repo diff hosts. Each holds only suspended rows
/// (bounded by the per-diff `MAX_FILE_*` caps), no watchers, so the ceiling is a
/// few tens of MB worst case; a session usually visits 1-3 repos so the cap is
/// rarely hit.
const DIFF_VIEW_CACHE_CAP: usize = 6;

/// Warm-resume cache key for a single-repo [`crate::diff::DiffView`]
/// (Project / Worktree scope). A diff entity is reused across a CLI↔Diff toggle
/// (or a workspace switch back to a visited repo) only when all three match: the
/// repo root, the scope, and the exact worktree set (column layout). A scope
/// toggle or a changed worktree set therefore correctly misses the cache and
/// mounts a structurally-correct entity - a stale layout can never render.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DiffViewKey {
    repo_root: PathBuf,
    scope: DiffScope,
    worktrees_hash: u64,
}

impl DiffViewKey {
    fn new(repo_root: &Path, scope: DiffScope, worktrees: &[DiffWorktree]) -> Self {
        use std::hash::{Hash as _, Hasher as _};
        // Hash the RAW path strings - NOT `norm_path` (which `fs::canonicalize`s,
        // a blocking syscall that would stall the GPUI thread on a slow/NFS mount
        // for every worktree on each rebuild). The key only needs to be stable per
        // (repo, scope, worktree set), and the seed paths already are; symlink
        // dedup is the column builder's job (`spawn_worktree_discovery`), not the
        // cache key's. Worst case a symlinked duplicate misses the warm cache.
        let mut paths: Vec<String> = worktrees
            .iter()
            .map(|w| w.path.to_string_lossy().into_owned())
            .collect();
        paths.sort();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        paths.hash(&mut h);
        Self {
            repo_root: repo_root.to_path_buf(),
            scope,
            worktrees_hash: h.finish(),
        }
    }
}

/// Worktree-scope curation filter: keep only the worktrees whose raw path is in
/// the chosen set. `None` (or an empty set) ⇒ keep ALL (the default). Raw path
/// strings (not `norm_path`) so there is no `canonicalize` syscall on the GPUI
/// thread and the match is stable with the picker + `diff_chosen_worktrees` keys.
fn filter_chosen(
    worktrees: Vec<DiffWorktree>,
    chosen: Option<&std::collections::HashSet<String>>,
) -> Vec<DiffWorktree> {
    match chosen {
        Some(set) if !set.is_empty() => worktrees
            .into_iter()
            .filter(|w| set.contains(&w.path.to_string_lossy().into_owned()))
            .collect(),
        _ => worktrees,
    }
}

/// Stable signature of the Multi-project repo-group set, so the retained
/// host is reused only while the same projects are open (order-insensitive).
fn multiproject_signature(groups: &[RepoGroup]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut entries: Vec<(String, Vec<String>)> = groups
        .iter()
        .map(|g| {
            let mut worktrees: Vec<String> = g
                .worktrees
                .iter()
                .map(|w| w.path.to_string_lossy().into_owned())
                .collect();
            worktrees.sort();
            (norm_path(&g.repo_root), worktrees)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut h);
    h.finish()
}

/// Width of the diff surface's own file-list column, per the design's
/// "a 210px file list". It used to be 360 - Zed's git-panel default, inherited
/// from when this was the width of the Diff rail. A rail is the window's list
/// and can afford the room; this list is a column *inside* one surface, and 360
/// of a pane's width is a third of the thing the user came to read.
pub(crate) const DIFF_FILE_LIST_WIDTH: f32 = 210.0;

impl SplitlaneApp {
    /// Show the active container's `Changes` surface. The keyboard path to the
    /// rail row of the same name.
    ///
    /// It opens and does not toggle. `FOCUS.md` gives this action one line -
    /// "by the ladder, starting from the diff pane, focus the diff pane" - and
    /// a toggle would have made the second press mean "hide it", which is what
    /// the pane header's `×` is for. Toggling was the shape of the full-area
    /// mode this replaced: back then the only way out of the diff was the same
    /// chord that opened it.
    pub(crate) fn handle_open_diff_view(
        &mut self,
        _: &OpenDiffView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.enter_diff_mode(cx);
    }

    /// Open the active container's diff surface. The keyboard/palette path to
    /// what the row's diff counters open with a click - the same call, so the
    /// surface appears in the rail either way.
    pub(crate) fn enter_diff_mode(&mut self, cx: &mut Context<Self>) {
        let ws_idx = self.active_idx;
        if ws_idx >= self.workspaces.len() {
            return;
        }
        self.open_container_diff_surface(ws_idx, cx);
    }

    /// (Re)point the mounted diff host to the active workspace's `repo_root` and
    /// the active scope. Warm-resume: rather than destroy + cold-rebuild
    /// every time, this parks the currently-displayed host (suspending its
    /// watchers while the cache retains its computed rows), prunes hosts of
    /// closed repos, then either RESUMES a cached entity whose key matches (the
    /// instant warm path - a CLI↔Diff toggle or a workspace switch back to a
    /// visited repo) or builds a fresh one on a cache miss. Clears to the
    /// empty-state when the active workspace has no resolved repo. Called on
    /// diff-mode entry, workspace switch, and scope change. The seed
    /// (`collect_*`) is a pure in-memory read; git subprocesses run off the
    /// main thread inside the entity.
    pub(crate) fn rebuild_diff_view(&mut self, cx: &mut Context<Self>) {
        // Park (suspend + unpoint) the displayed host into the cache before
        // re-pointing, then drop cached hosts whose repo closed.
        self.park_displayed_diff(cx);
        self.prune_diff_cache();

        let repo_root = self
            .workspaces
            .get(self.active_idx)
            .and_then(|ws| ws.repo_root.clone());

        match self.diff_mode.diff_scope {
            // One host with a tab per repo, lazy-mounting each repo's
            // DiffView. Retained across toggles while the project set is stable.
            DiffScope::MultiProject => {
                self.diff_mode.diff_view_key = None;
                let groups = self.collect_multiproject_groups();
                if groups.is_empty() {
                    self.diff_mode.multi_diff_view_retained = None;
                    cx.notify();
                    return;
                }
                let sig = multiproject_signature(&groups);
                if let Some((retained_sig, view)) = self.diff_mode.multi_diff_view_retained.clone()
                    && retained_sig == sig
                {
                    view.update(cx, |v, cx| v.resume(cx));
                    self.diff_mode.multi_diff_view = Some(view);
                    cx.notify();
                    return;
                }
                let view = cx.new(|cx| crate::diff::MultiRepoDiffView::new(groups, cx));
                self.diff_mode.multi_diff_view_retained = Some((sig, view.clone()));
                self.diff_mode.multi_diff_view = Some(view);
            }
            // The active workspace only (one column).
            DiffScope::Project => {
                let Some(root) = repo_root else {
                    self.diff_mode.diff_view_key = None;
                    cx.notify();
                    return;
                };
                let worktrees = self.collect_project_worktrees();
                let key = DiffViewKey::new(&root, DiffScope::Project, &worktrees);
                let (view, _miss) = self.mount_or_resume_diff(key, root, worktrees, cx);
                self.diff_mode.diff_view = Some(view);
            }
            // Open worktrees of the active repo; on a COLD mount they are
            // augmented off-thread with on-disk worktrees not open as workspaces.
            // A cache hit already carries those discovered columns, so discovery
            // re-runs only on a miss.
            DiffScope::Worktree => {
                let Some(root) = repo_root else {
                    self.diff_mode.diff_view_key = None;
                    cx.notify();
                    return;
                };
                // Curation: if the user chose a subset of branches for this repo,
                // build columns for exactly those (unchosen branches are never
                // diffed); no choice ⇒ all worktrees (the default).
                let chosen = self.diff_mode.diff_chosen_worktrees.get(&root).cloned();
                let open = filter_chosen(self.collect_diff_worktrees(&root), chosen.as_ref());
                let key = DiffViewKey::new(&root, DiffScope::Worktree, &open);
                let (view, miss) = self.mount_or_resume_diff(key, root.clone(), open.clone(), cx);
                self.diff_mode.diff_view = Some(view);
                // Eagerly fetch this repo's worktree list (off the
                // main thread) so the scope header's branch badge can read
                // "chosen/total" without the picker ever being opened. Guarded so
                // it fetches at most once per repo (the picker-open path refreshes
                // it too); the data is a best-effort hint, not load-bearing.
                if self.diff_mode.diff_available_repo.as_deref() != Some(root.as_path()) {
                    self.refresh_diff_available_worktrees(root.clone(), cx);
                }
                if miss {
                    self.spawn_worktree_discovery(root, open, chosen, cx);
                }
            }
        }
        cx.notify();
    }

    /// Park the displayed diff host before re-pointing (mode exit,
    /// workspace switch, scope change). Suspends the entity - releasing its OS
    /// watchers + ending its debounce loop - while the cache (single-repo) or
    /// the retained slot (Multi-project) keeps it alive with its computed rows.
    /// Clears only the display pointer (pointer-vs-owner split).
    /// Also closes the prior `multi_diff_view` watcher leak (it was never
    /// cleared on CLI/Agents entry).
    pub(crate) fn park_displayed_diff(&mut self, cx: &mut Context<Self>) {
        if let Some(dv) = self.diff_mode.diff_view.take() {
            dv.update(cx, |v, cx| v.suspend(cx));
        }
        if let Some(mv) = self.diff_mode.multi_diff_view.take() {
            mv.update(cx, |v, cx| v.suspend(cx));
        }
    }

    /// Resume + return the cached single-repo `DiffView` on a key hit
    /// (instant warm rows + a cheap fingerprint revalidation), or build, insert,
    /// and return a fresh one on a miss. Returns `(entity, was_miss)` so the
    /// Worktree branch knows whether to (re)run on-disk discovery. Sets
    /// `diff_view_key` to the mounted key.
    fn mount_or_resume_diff(
        &mut self,
        key: DiffViewKey,
        root: PathBuf,
        worktrees: Vec<DiffWorktree>,
        cx: &mut Context<Self>,
    ) -> (Entity<crate::diff::DiffView>, bool) {
        if let Some(view) = self.diff_mode.diff_view_cache.get(&key).cloned() {
            view.update(cx, |v, cx| v.resume(cx));
            self.diff_mode.diff_view_key = Some(key);
            return (view, false);
        }
        let view = cx.new(|cx| crate::diff::DiffView::new(root, worktrees, cx));
        // Worktree scope: the column-header `×` deselects the branch (rebuild
        // without it) instead of hiding it in place - shown-or-not, no "N hidden"
        // limbo. Wire it once on the fresh entity; the subscription + flag persist
        // across cache resume (the key is scope-stamped, so a worktree view is
        // never reused for another scope).
        if self.diff_mode.diff_scope == DiffScope::Worktree {
            view.update(cx, |v, _| v.set_close_removes(true));
            cx.subscribe(&view, Self::handle_diff_view_event).detach();
        }
        self.diff_mode
            .diff_view_cache
            .insert(key.clone(), view.clone());
        self.diff_mode.diff_view_key = Some(key.clone());
        self.evict_diff_cache_if_needed(&key);
        (view, true)
    }

    /// Drop cached diff hosts whose repo is no longer backed by any open
    /// workspace (closed project). Dropping the (already-suspended) entity frees
    /// its retained rows; its watchers were released on suspend. Keeps the cache
    /// bounded to live repos.
    fn prune_diff_cache(&mut self) {
        let open: std::collections::HashSet<PathBuf> = self
            .workspaces
            .iter()
            .filter_map(|ws| ws.repo_root.clone())
            .collect();
        self.diff_mode
            .diff_view_cache
            .retain(|k, _| open.contains(&k.repo_root));
        if open.is_empty() {
            self.diff_mode.multi_diff_view_retained = None;
        }
    }

    /// Bound the cache to `DIFF_VIEW_CACHE_CAP`. Every non-displayed
    /// entry is suspended (no watchers, just rows), so order-insensitive
    /// eviction is safe; `keep` (the just-mounted key) is never evicted.
    /// Dropping an entity frees its rows.
    fn evict_diff_cache_if_needed(&mut self, keep: &DiffViewKey) {
        if self.diff_mode.diff_view_cache.len() <= DIFF_VIEW_CACHE_CAP {
            return;
        }
        let victims: Vec<DiffViewKey> = self
            .diff_mode
            .diff_view_cache
            .keys()
            .filter(|k| *k != keep)
            .cloned()
            .collect();
        for k in victims {
            if self.diff_mode.diff_view_cache.len() <= DIFF_VIEW_CACHE_CAP {
                break;
            }
            self.diff_mode.diff_view_cache.remove(&k);
        }
    }

    /// Off the main thread, enumerate the repo's on-disk worktrees and
    /// append any not already open as workspaces (dedup by a case-safe
    /// normalized path) to the live `DiffView` *in place* via `add_columns` -
    /// no re-mount, so existing columns keep their loaded content (no
    /// Loading→Loading flash mid-session). Discovered-only worktrees carry
    /// `workspace_id: None`. No-op when nothing new is found.
    fn spawn_worktree_discovery(
        &mut self,
        root: std::path::PathBuf,
        open: Vec<crate::diff::DiffWorktree>,
        chosen: Option<std::collections::HashSet<String>>,
        cx: &mut Context<Self>,
    ) {
        // Flag the in-flight discovery so the sidebar shows a
        // "Discovering worktrees…" note instead of looking like columns are
        // missing during the brief cold-mount window.
        self.diff_mode.diff_discovering = true;
        self.diff_mode.diff_discovering_root = Some(root.clone());
        let requested_root = root.clone();
        cx.spawn(async move |this, cx| {
            let discovered =
                smol::unblock(move || crate::diff::list_repo_worktrees(&requested_root)).await;
            let mut seen: std::collections::HashSet<String> =
                open.iter().map(|w| norm_path(&w.path)).collect();
            let mut new_cols = Vec::new();
            for (path, branch) in discovered {
                // Curation: when the user chose a subset, only append discovered
                // worktrees that ARE in the chosen set (raw path key, matching the
                // picker + `filter_chosen`).
                if let Some(set) = &chosen
                    && !set.contains(&path.to_string_lossy().into_owned())
                {
                    continue;
                }
                if seen.insert(norm_path(&path)) {
                    new_cols.push(crate::diff::DiffWorktree {
                        path,
                        branch,
                        workspace_id: None,
                    });
                }
            }
            let _ = cx.update(|cx| {
                this.update(cx, |app, cx| {
                    let owns_discovery =
                        app.diff_mode.diff_discovering_root.as_deref() == Some(root.as_path());
                    let still_current_worktree_view = app.active_container_shows_a_diff(cx)
                        && app.diff_mode.diff_scope == crate::diff::DiffScope::Worktree
                        && app.diff_mode.diff_view_key.as_ref().is_some_and(|key| {
                            key.repo_root == root && key.scope == crate::diff::DiffScope::Worktree
                        });
                    if owns_discovery {
                        app.diff_mode.diff_discovering = false;
                        app.diff_mode.diff_discovering_root = None;
                    }
                    // Apply only if still showing this repo's worktree scope.
                    if !new_cols.is_empty()
                        && still_current_worktree_view
                        && owns_discovery
                        && let Some(dv) = app.diff_mode.diff_view.clone()
                    {
                        dv.update(cx, |v, cx| v.add_columns(new_cols, cx));
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }

    /// Worktree-scope branches picker: (re)fetch every worktree of `root` off the
    /// main thread so the picker can offer branches not currently shown (it lists
    /// the full set; columns show only the chosen subset). Lazy - called when the
    /// picker opens.
    pub(crate) fn refresh_diff_available_worktrees(
        &mut self,
        root: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.diff_mode.diff_available_repo = Some(root.clone());
        let requested_root = root.clone();
        cx.spawn(async move |this, cx| {
            let wts =
                smol::unblock(move || crate::diff::list_repo_worktrees(&requested_root)).await;
            let _ = cx.update(|cx| {
                this.update(cx, |app, cx| {
                    if app.diff_mode.diff_available_repo.as_deref() != Some(root.as_path()) {
                        return;
                    }
                    app.diff_mode.diff_available_worktrees = wts
                        .into_iter()
                        .map(|(path, branch)| crate::diff::DiffWorktree {
                            path,
                            branch,
                            workspace_id: None,
                        })
                        .collect();
                    cx.notify();
                })
            });
        })
        .detach();
    }

    /// Worktree-scope branches picker: toggle `path` in/out of `root`'s chosen
    /// worktree set, then rebuild so the column set follows. The first toggle
    /// materializes the implicit "all" into an explicit set (so unchecking one
    /// from "all" works); emptying the set reverts to the all-default (a zero-
    /// column view is meaningless).
    pub(crate) fn toggle_chosen_worktree(
        &mut self,
        root: std::path::PathBuf,
        path: String,
        cx: &mut Context<Self>,
    ) {
        let all: std::collections::HashSet<String> = self
            .diff_mode
            .diff_available_worktrees
            .iter()
            .map(|w| w.path.to_string_lossy().into_owned())
            .collect();
        let set = self
            .diff_mode
            .diff_chosen_worktrees
            .entry(root.clone())
            .or_insert(all);
        if !set.remove(&path) {
            set.insert(path);
        }
        let now_empty = set.is_empty();
        if now_empty {
            self.diff_mode.diff_chosen_worktrees.remove(&root);
        }
        self.rebuild_diff_view(cx);
    }

    /// Whether `path` is currently shown as a column (in the chosen set, or no
    /// chosen set ⇒ all shown). Drives the branches-picker checkmarks.
    pub(crate) fn diff_worktree_is_chosen(&self, root: &std::path::Path, path: &str) -> bool {
        match self.diff_mode.diff_chosen_worktrees.get(root) {
            Some(set) => set.contains(path),
            None => true,
        }
    }

    /// Worktree scope: a branch column asked to close (its header `×`). Drop it
    /// from the chosen set and rebuild - same selection model as the branches
    /// picker, so re-checking it there brings it back. No "hidden" limbo.
    pub(crate) fn handle_diff_view_event(
        &mut self,
        _view: Entity<DiffView>,
        event: &DiffViewEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            DiffViewEvent::CloseColumn { path } => {
                self.deselect_diff_worktree(path.to_string_lossy().into_owned(), cx);
            }
        }
    }

    /// Remove `path` from the active repo's chosen-worktree set, then rebuild so
    /// the column disappears. The "all-shown" default is materialized from the
    /// columns currently on screen (so on-disk-discovered branches survive), then
    /// `path` is dropped. No-op when only one column remains - a zero-column diff
    /// is meaningless (mirrors [`Self::toggle_chosen_worktree`]'s empty guard).
    fn deselect_diff_worktree(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(root) = self
            .workspaces
            .get(self.active_idx)
            .and_then(|ws| ws.repo_root.clone())
        else {
            return;
        };
        let shown: std::collections::HashSet<String> = self
            .diff_mode
            .diff_view
            .as_ref()
            .map(|v| {
                v.read(cx)
                    .column_paths()
                    .into_iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        if shown.len() <= 1 {
            return;
        }
        let set = self
            .diff_mode
            .diff_chosen_worktrees
            .entry(root)
            .or_insert(shown);
        set.remove(&path);
        self.rebuild_diff_view(cx);
    }

    /// The design's `⤢` on the Review header: give the review the whole
    /// window, and give the window back on the second press.
    ///
    /// Two things are displaced, and both are recorded rather than recomputed -
    /// "restores the previous panes" can only mean the same thing twice if the
    /// previous state is the thing that was saved:
    ///
    /// - the Files panel is closed, because the review has a file list of its
    ///   own and two lists of changed files on one screen is what the merged
    ///   rail exists to prevent;
    /// - the container's panes collapse to the one showing the review, through
    ///   the same zoom the `⌥\` gesture uses, so nothing is closed and no PTY
    ///   is touched.
    ///
    /// When the review IS the content area - the container's parked `Changes`
    /// surface, selected from the rail - there are no panes to collapse, and
    /// the gesture is the Files panel alone.
    pub(crate) fn handle_diff_full_window(
        &mut self,
        _: &crate::DiffFullWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(saved) = self.diff_mode.diff_full_window.take() {
            if let Some(container_id) = saved.zoomed_container
                && let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == container_id)
                && ws.is_zoomed()
                && let Some(pane) = ws.exit_zoom(cx)
            {
                pane.read(cx).focus_handle(cx).focus(window, cx);
            }
            if saved.files_sidebar_was_open && !self.files_sidebar_open {
                self.toggle_files_sidebar(cx);
            }
            self.save_session(cx);
            cx.notify();
            return;
        }

        let files_sidebar_was_open = self.files_sidebar_open;
        let zoomed_container = self.zoom_to_review_pane(window, cx);
        if files_sidebar_was_open {
            self.close_files_sidebar(cx);
        }
        self.diff_mode.diff_full_window = Some(crate::DiffFullWindowState {
            files_sidebar_was_open,
            zoomed_container,
        });
        self.save_session(cx);
        cx.notify();
    }

    /// Collapse the active container's panes to the one showing a diff, and
    /// report which container that was. `None` when there is nothing to
    /// collapse: one pane already, no pane showing a diff, or the review is the
    /// container's parked surface rather than a slot's.
    fn zoom_to_review_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<u64> {
        let ws = self.active_workspace_mut()?;
        if ws.is_zoomed() {
            return None;
        }
        let root = ws.root.as_ref()?;
        if root.leaf_count() <= 1 {
            return None;
        }
        let target = root.collect_leaves().into_iter().find(|pane| {
            pane.read(cx)
                .tabs
                .iter()
                .any(|tab| matches!(tab, crate::pane::TabContent::Diff(_)))
        })?;
        let container_id = ws.id;
        target.update(cx, |p, _| p.zoomed = true);
        let full_tree = ws.root.take()?;
        ws.saved_layout = Some(full_tree);
        ws.root = Some(crate::layout::LayoutTree::Leaf(target.clone()));
        target.read(cx).focus_handle(cx).focus(window, cx);
        Some(container_id)
    }

    /// Push the diff surface's own chrome into every diff pane of the active
    /// container: the scope breadcrumb and the 210px file list.
    ///
    /// Both are built from app-level state - the scope, the branch/revision
    /// filter, the search box - and a `DiffView` reads no app state, so they
    /// travel the push-only route `scope_slot` already used. This used to be
    /// the full-area render branch's job, composing the column and the entity
    /// side by side; with the diff living in a pane there is no wrapper to
    /// compose in, and the column belongs inside the surface anyway.
    ///
    /// Called once per frame from `render`, beside `sync_surface_facts`.
    pub(crate) fn sync_diff_panes(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.active_workspace().and_then(|ws| ws.root.as_ref()) else {
            return;
        };
        let diffs: Vec<gpui::Entity<crate::diff::DiffView>> = root
            .collect_leaves()
            .into_iter()
            .flat_map(|pane| {
                pane.read(cx)
                    .tabs
                    .iter()
                    .filter_map(|tab| match tab {
                        crate::pane::TabContent::Diff(view) => Some(view.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        if diffs.is_empty() {
            return;
        }
        for view in diffs {
            let breadcrumb = self.render_scope_header(cx);
            let files = self.render_diff_file_column(cx);
            view.update(cx, |view, _| {
                view.scope_slot = Some(breadcrumb);
                view.files_slot = Some(files);
            });
        }
    }
}

/// Normalize a worktree path for dedup so the same worktree reported by
/// `git worktree list` and by an open workspace collapses to one entry. Resolves
/// symlinks via `canonicalize` (falling back to the raw path) and lowercases on
/// case-insensitive filesystems (macOS / Windows) so a case-different path does
/// not produce a duplicate column.
fn norm_path(p: &std::path::Path) -> String {
    let resolved = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = resolved.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        s.to_lowercase()
    } else {
        s
    }
}
