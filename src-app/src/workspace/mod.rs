//! Workspace - a named collection of terminal panes with a split layout.
//!
//! Module layout:
//! - [`git`] - git metadata probing (branch, diff stats, `.git` dir lookup)
//! - [`ports`] - cross-platform TCP listening-port detection
//!
//! The [`Workspace`] struct and its constructors live in this `mod.rs`; git
//! and port helpers are re-exported so external callers keep the flat
//! `crate::workspace::*` API.

pub(crate) mod git;
pub mod pid_resolve;
mod ports;
pub mod surface_naming;
pub mod worktree;

pub use git::{
    GitDiffStats, GitMark, detect_branch, find_git_dir, resolve_repo_root,
    status_marks as git_status_marks,
};
#[cfg(test)]
pub(crate) use ports::PortEntry;
pub use ports::{PaneScan, scan_panes};

/// Hard cap on open containers (single source for the bound previously
/// re-declared as a local `const` at every create/IPC site).
///
/// One number, because there is one list. The rails used to cap their two
/// views of it separately - 20 for the CLI rail, 128 for the Agents rail - so
/// merging at the lower bound would have dropped containers a user already
/// had. It is the higher of the two, and it governs both restore and create.
pub(crate) const MAX_WORKSPACES: usize = 128;

use gpui::{App, Entity, Window};
use splitlane_config::schema::{ButtonCommand, LayoutNode};

use crate::ai_types::AgentSession;
use crate::launch_cwd;
use crate::layout::LayoutTree;
use crate::pane::Pane;

use self::git::parse_head;

/// The process's one monotonic id counter.
///
/// Containers and surfaces used to draw from three separate counters that all
/// started at 1, so their ids collided by construction and the PTY env id had
/// to offset one namespace out of the other's way. One counter is what makes
/// `SPLITLANE_WORKSPACE_ID` a single unambiguous number again: a workspace id
/// and a thread id can no longer name the same thing.
///
/// Ids restored from `session.json` are persistent; the counter is advanced
/// past all of them (`crate::project::max_persisted_id`) before anything new is
/// minted, so a fresh id can never land on a restored one.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Take the next id. See [`NEXT_ID`].
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Advance the shared counter so it leads `target`. Idempotent; a no-op when
/// the counter is already ahead.
pub fn bump_next_id_to(target: u64) {
    bump_counter(&NEXT_ID, target);
}

/// Raise `counter` to `target`, never lowering it. Split out so it can be
/// exercised on a local atomic instead of the process-wide one.
pub(crate) fn bump_counter(counter: &std::sync::atomic::AtomicU64, target: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut current = counter.load(Relaxed);
    while current < target {
        match counter.compare_exchange_weak(current, target, Relaxed, Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

/// A workspace's id. Ephemeral - re-minted on every restore - but drawn from
/// the same counter as every other id, so it is unique process-wide.
pub fn next_workspace_id() -> u64 {
    next_id()
}

/// Runtime-only notification state for a completed agent turn.
///
/// A natural `ai.stop` marks the completion unread only while this workspace is
/// not visible in the active Splitlane window. It survives the transient
/// `AgentState::Finished` session auto-clear until the user interacts with the
/// workspace card or its pane area.
#[derive(Debug, Default)]
pub(crate) struct AgentCompletionNotification {
    unread: bool,
}

impl AgentCompletionNotification {
    pub(crate) fn record_finished(&mut self, workspace_visible: bool) {
        self.unread = !workspace_visible;
    }

    pub(crate) fn acknowledge(&mut self) {
        self.unread = false;
    }

    pub(crate) fn is_unread(&self) -> bool {
        self.unread
    }
}

/// One container: a directory the user has open.
///
/// The CLI rail's workspace and the Agents rail's project were two runtime
/// types for one thing; this is what they merged into. It owns both a slot
/// tree ([`Self::root`]) and the parked agent surfaces ([`Self::threads`]) -
/// the two shapes the old rails each showed half of.
pub struct Workspace {
    /// Unique container identifier, assigned at construction.
    pub id: u64,
    pub title: String,
    /// Working directory at creation time. Does not update when the shell `cd`s.
    pub cwd: String,
    pub root: Option<LayoutTree>,
    /// Saved layout tree when zoomed. `Some(tree)` means the workspace is zoomed
    /// and `root` contains only the zoomed pane as a single Leaf.
    pub saved_layout: Option<LayoutTree>,
    /// Cached git diff stats, refreshed by a background poller.
    pub git_stats: GitDiffStats,
    /// Current git branch name. Empty string when not a git repo or branch unknown.
    pub git_branch: String,
    /// Whether this workspace's CWD is inside a git repository.
    pub is_git_repo: bool,
    /// Resolved `.git` directory path (for file watcher). `None` if not a git repo.
    pub git_dir: Option<std::path::PathBuf>,
    /// Working directory of the shared repository (parent of the *main* `.git`),
    /// canonicalized. Sibling worktrees of one repo share an identical value -
    /// the invariant the sidebar uses to group them. `None` when not a git repo.
    pub repo_root: Option<std::path::PathBuf>,
    /// Whether this workspace's CWD is a *linked* git worktree (as opposed to
    /// the repo's main checkout). Linked worktrees carry a `commondir` file.
    // Read to target git operations at the worktree root and for column
    // labeling; stored at construction.
    #[allow(dead_code)]
    pub is_worktree: bool,
    /// Concrete worktree checkout root resolved at workspace construction.
    /// Review UI reads this directly so rebuilding columns stays in-memory.
    pub worktree_root: std::path::PathBuf,
    /// Active TCP listening ports from workspace terminal processes.
    pub active_ports: Vec<u16>,
    /// Generation counter for event-driven port scans - the cancellation
    /// belt for workspace close/reuse (superseded scans check it to abort).
    pub port_scan_generation: u64,
    /// True while a scan ladder (debounce + retries) is in flight for this
    /// workspace - ActivityBursts arriving meanwhile are absorbed instead of
    /// superseding the pending scan (under sustained output, the old
    /// generation-bump-per-burst starved the 500ms debounce indefinitely).
    pub port_scan_pending: bool,
    /// Service metadata for `active_ports` chips, fed from BOTH sides:
    /// OS-side argv classification (authoritative for `is_frontend`, with a
    /// synthesized localhost URL) and PTY-output detection (enrichment -
    /// exact URL with path, backend labels). Keyed by port number; pruned
    /// when ports are removed from `active_ports`.
    pub service_labels: std::collections::HashMap<u16, crate::terminal::ServiceInfo>,
    /// Registered AI agent sessions for this workspace, keyed by PID. A
    /// workspace can hold many concurrent sessions (e.g., two Claude
    /// Codes + one Codex) - the sidebar aggregates them per tool with
    /// `ai_types::aggregate_by_tool`. Cleaned up by the stale-PID sweep
    /// in `event_handlers::sweep_stale_pids`.
    pub agent_sessions: std::collections::HashMap<u32, AgentSession>,
    /// Persistent-in-session completion notification shown as a blue dot in
    /// the Workspaces sidebar until the user interacts with this workspace.
    pub(crate) agent_completion_notification: AgentCompletionNotification,
    /// AI agent process basenames detected by walking the workspace's
    /// PTY descendants (Linux `/proc/<pid>/comm`, macOS `libproc::name`).
    /// Independent of the optional IPC hook handshake -- this is what
    /// the sidebar pastille reads so the "session active" signal works
    /// even when Claude Code is launched without the Splitlane shim.
    /// Refreshed by the per-pane `scan_panes` walk - the
    /// union of every pane's detected agents; the recognition vocabulary
    /// is `TerminalAgent::ALL` binaries (16), unified from the historical
    /// 3-name `AI_PROCESS_NAMES` list.
    pub detected_agents: std::collections::HashSet<String>,
    /// User-defined tab-bar buttons for this workspace.
    /// Rendered after the 2 built-in defaults (Claude / Codex).
    pub custom_buttons: Vec<ButtonCommand>,
    /// Absolute directory paths expanded in the Files tree sidebar, held
    /// per-workspace so reopening the sidebar (within a session or after a
    /// restart) restores the same expansion. Excludes
    /// the implicit root. Persisted as workspace-relative paths in
    /// `session.json`; the sidebar's visibility itself is never persisted.
    pub files_expanded: Vec<std::path::PathBuf>,
    /// Git worktrees Splitlane created for this workspace's panes via
    /// `splitlane up` (`worktree = "branch"`). Torn
    /// down - clean ones only, branch never deleted - when the workspace
    /// closes; persisted in `session.json` so a crash keeps the ownership
    /// record. Empty for every workspace not built by `up` with worktrees.
    pub managed_worktrees: Vec<worktree::ManagedWorktree>,
    /// What the rail's `+ worktree` runs inside a worktree it has just made,
    /// remembered for this project so it is typed once per repository rather
    /// than once per worktree.
    ///
    /// The preset carries the same thing per pane (`PanePreset::setup`), and
    /// this is the rail's answer to the same question: a bootstrap command is
    /// a property of the repository - `npm install`, `bundle install`, a
    /// script that writes an `.env` - not of the branch somebody just cut.
    /// `None` and `Some("")` both mean "run nothing"; the dialog stores the
    /// trimmed value, so only `None` is ever written.
    ///
    /// Persisted in `session.json` beside [`Self::preferred_agent`], the
    /// other per-project choice, and for the same reason: a choice a person
    /// made once must not be re-asked on the next launch.
    pub worktree_setup: Option<String>,
    /// Agent surfaces parked on this container - what the Agents rail used to
    /// call threads. They are not in [`Self::root`]: a thread is shown
    /// full-area, never inside the slot tree, so it has no leaf to live in.
    /// Merged in when the runtime containers merged; the persisted record has
    /// carried them since schema v2.
    pub threads: Vec<crate::project::Thread>,
    /// The container's diff surface, once it has been opened: its stable id,
    /// drawn from the same counter as every other surface.
    ///
    /// `None` means "not open", and that is the default. `Changes` used to
    /// appear under every container whether anyone had asked for it or not -
    /// a permanent menu entry drawn as a surface row. It is an ordinary
    /// surface: it opens (from the row's diff counters, the container's
    /// create menu, or the palette), it shows up in the rail after that, and
    /// it closes.
    pub diff_surface: Option<u64>,
    /// Whether the rail row is expanded, showing this container's surfaces.
    pub is_expanded: bool,
    /// Which agent the new-agent chord starts in this container, when this
    /// container has an answer of its own.
    ///
    /// `None` is **"never asked"**, not "always ask" - it inherits the
    /// app-level `default_agent`; a person who wants to be asked every time
    /// says so with [`PreferredAgent::Ask`]. Per container because that is
    /// where the answer is stable: one repository is worked on with one agent
    /// far more often than not.
    ///
    /// Nothing writes this but a choice made to write it. See
    /// [`crate::agent_launcher::PreferredAgent`].
    pub preferred_agent: Option<crate::agent_launcher::PreferredAgent>,
    /// The rail group this project is in, by `ProjectGroup::id`. `None` is no
    /// group, and so is an id no group carries. See `app::project_groups`.
    pub group: Option<u64>,
}

impl Workspace {
    /// Shared private factory for the three public constructors (kills
    /// the verbatim triplication). Resolves the *cheap* git metadata - `.git`
    /// dir, branch (`parse_head`), repo root - synchronously, since those are
    /// direct `.git/HEAD` file reads, not subprocesses. `git_stats` is left at
    /// its `default()` (0/0): the `git diff --shortstat` subprocess is the
    /// blocking call, deferred off the render thread by
    /// [`crate::SplitlaneApp::spawn_initial_git_stats`] right after creation.
    fn build(id: u64, title: String, cwd: String, root: Option<LayoutTree>) -> Self {
        let git_dir = find_git_dir(&cwd);
        let (git_branch, is_git_repo) = match &git_dir {
            Some(dir) => parse_head(dir),
            None => (String::new(), false),
        };
        let (repo_root, is_worktree) = match &git_dir {
            Some(dir) => resolve_repo_root(dir),
            None => (None, false),
        };
        let worktree_root =
            git::resolve_worktree_root(&cwd, git_dir.as_deref(), repo_root.as_deref(), is_worktree);
        Self {
            id,
            title,
            cwd,
            root,
            saved_layout: None,
            git_stats: GitDiffStats::default(),
            git_branch,
            is_git_repo,
            git_dir,
            repo_root,
            is_worktree,
            worktree_root,
            active_ports: vec![],
            port_scan_generation: 0,
            port_scan_pending: false,
            service_labels: std::collections::HashMap::new(),
            agent_sessions: std::collections::HashMap::new(),
            agent_completion_notification: AgentCompletionNotification::default(),
            detected_agents: std::collections::HashSet::new(),
            custom_buttons: Vec::new(),
            files_expanded: Vec::new(),
            managed_worktrees: Vec::new(),
            worktree_setup: None,
            threads: Vec::new(),
            diff_surface: None,
            is_expanded: true,
            preferred_agent: None,
            group: None,
        }
    }

    /// Create a workspace with a pre-allocated ID (use `next_workspace_id()` to obtain one).
    pub fn with_id(id: u64, title: impl Into<String>, pane: Entity<Pane>) -> Self {
        let cwd = launch_cwd::implicit_launch_cwd().display().to_string();
        Self::build(id, title.into(), cwd, Some(LayoutTree::Leaf(pane)))
    }

    /// Create a workspace with a pre-allocated ID and explicit CWD.
    pub fn with_cwd_and_id(
        id: u64,
        title: impl Into<String>,
        cwd: std::path::PathBuf,
        pane: Entity<Pane>,
    ) -> Self {
        Self::build(
            id,
            title.into(),
            cwd.display().to_string(),
            Some(LayoutTree::Leaf(pane)),
        )
    }

    /// Create a workspace with a pre-allocated ID and layout tree.
    pub fn with_layout_and_id(
        id: u64,
        title: impl Into<String>,
        cwd: std::path::PathBuf,
        root: LayoutTree,
    ) -> Self {
        Self::build(id, title.into(), cwd.display().to_string(), Some(root))
    }

    /// A container with no panes - the state a container is left in by
    /// closing its last one, and the state it comes back in.
    ///
    /// Not a defect and not a placeholder: the rail row's own `+ agent` /
    /// `+ shell` / `+ worktree` are how a pane arrives, and inventing one
    /// here would put back the shell the user had just closed.
    pub fn empty_with_id(id: u64, title: impl Into<String>, cwd: std::path::PathBuf) -> Self {
        Self::build(id, title.into(), cwd.display().to_string(), None)
    }

    /// A container with no slot tree - the shape the rail data tests need,
    /// since building a real one takes GPUI entities.
    #[cfg(test)]
    pub(crate) fn detached(title: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self::build(next_id(), title.into(), cwd.into(), None)
    }

    pub fn is_zoomed(&self) -> bool {
        self.saved_layout.is_some()
    }

    pub fn exit_zoom(&mut self, cx: &mut App) -> Option<Entity<Pane>> {
        let zoomed_pane = self.root.as_ref().and_then(|root| root.first_leaf());
        let saved = self.saved_layout.take()?;
        self.root = Some(saved);
        if let Some(pane) = &zoomed_pane {
            pane.update(cx, |pane, _| {
                pane.zoomed = false;
            });
        }
        zoomed_pane
    }

    pub fn pane_count(&self) -> usize {
        self.root.as_ref().map_or(0, |r| r.leaf_count())
    }

    pub fn contains_pane(&self, pane: &Entity<Pane>) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| root.contains_leaf(pane))
            || self
                .saved_layout
                .as_ref()
                .is_some_and(|saved| saved.contains_leaf(pane))
    }

    pub fn any_pane(&self, mut f: impl FnMut(&Entity<Pane>) -> bool) -> bool {
        if let Some(root) = &self.root
            && root.any_leaf(&mut f)
        {
            return true;
        }
        if let Some(saved) = &self.saved_layout
            && saved.any_leaf(&mut f)
        {
            return true;
        }
        false
    }

    pub fn collect_panes(&self) -> Vec<Entity<Pane>> {
        let mut panes = Vec::new();
        if let Some(root) = &self.root {
            panes.extend(root.collect_leaves());
        }
        if let Some(saved) = &self.saved_layout {
            for pane in saved.collect_leaves() {
                if !panes.contains(&pane) {
                    panes.push(pane);
                }
            }
        }
        panes
    }

    pub fn focus_first(&self, window: &mut Window, cx: &mut App) {
        if let Some(root) = &self.root {
            root.focus_first(window, cx);
        }
    }

    /// Serialize the workspace layout to a `LayoutNode`.
    ///
    /// When zoomed, serializes the saved (un-zoomed) layout so that the full
    /// pane arrangement is captured rather than just the single zoomed pane.
    pub fn serialize_layout(&self, cx: &App) -> Option<LayoutNode> {
        let tree = self.saved_layout.as_ref().or(self.root.as_ref())?;
        Some(tree.serialize(cx))
    }

    /// Serialize the workspace for session persistence without terminal
    /// output, which must remain local to the current process.
    pub fn serialize_layout_without_scrollback(&self, cx: &App) -> Option<LayoutNode> {
        let tree = self.saved_layout.as_ref().or(self.root.as_ref())?;
        Some(tree.serialize_without_scrollback(cx))
    }
}

impl Workspace {
    /// Push a refreshed [`SplitlaneConfig`] to every `Pane` in the
    /// workspace's layout so the tab bar re-renders against the new config
    /// without a per-frame `load_config()`. Called from
    /// `SplitlaneApp::process_config_changes` on every ConfigWatcher reload.
    pub fn propagate_config(
        &self,
        config: &splitlane_config::schema::SplitlaneConfig,
        cx: &mut App,
    ) {
        if let Some(root) = &self.root {
            walk_and_push_config(root, config, cx);
        }
        if let Some(saved) = &self.saved_layout {
            walk_and_push_config(saved, config, cx);
        }
    }
}

fn walk_and_push_config(
    node: &LayoutTree,
    config: &splitlane_config::schema::SplitlaneConfig,
    cx: &mut App,
) {
    match node {
        LayoutTree::Leaf(pane) => {
            pane.update(cx, |p, cx| {
                p.apply_config(config, cx);
            });
        }
        LayoutTree::Container { children, .. } => {
            for child in children {
                walk_and_push_config(&child.node, config, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentCompletionNotification;

    #[test]
    fn agent_completion_is_unread_only_while_workspace_is_not_visible() {
        let mut notification = AgentCompletionNotification::default();
        assert!(!notification.is_unread());

        notification.record_finished(false);
        assert!(notification.is_unread());

        notification.record_finished(true);
        assert!(!notification.is_unread());

        notification.record_finished(false);
        notification.acknowledge();
        assert!(!notification.is_unread());
    }
}
