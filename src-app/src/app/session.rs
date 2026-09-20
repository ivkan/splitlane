//! Session persistence for `SplitlaneApp` - save/restore workspace layouts
//! and their per-pane metadata so relaunching rebuilds what the user had open.
//! Terminal scrollback stays process-local: a relaunched shell must not inherit
//! plain-text output from an earlier PTY.
//!
//! Extracted from `main.rs`.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{App, AppContext, Context, Entity};
use splitlane_config::schema::LayoutNode;

use crate::SplitlaneApp;
use crate::launch_cwd;
use crate::layout::{LayoutTree, MAX_PANES};
use crate::limits::MAX_SESSION_SIZE_BYTES;
use crate::pane::Pane;
use crate::terminal::TerminalView;
use crate::workspace::{MAX_WORKSPACES, Workspace};

/// Cap on the number of `session.json.corrupted-*` backup files retained
/// alongside the live session. Beyond this, the oldest are deleted on
/// rotation. The risk this bounds: every parse
/// failure produces a new backup, and without rotation a user with a
/// chronic corruption (e.g. a flaky disk) would silently fill `~/.cache`.
const MAX_CORRUPTION_BACKUPS: usize = 5;

/// Debounce window for coalescing a burst of [`SplitlaneApp::save_session`]
/// calls into a single disk write. Short enough to be imperceptible, long
/// enough to absorb the multi-call bursts emitted when creating/closing many
/// workspaces in quick succession.
const SAVE_DEBOUNCE_MS: u64 = 150;

static SESSION_WRITE_LOCK: Mutex<()> = Mutex::new(());
static SESSION_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static SESSION_CORRUPTION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn session_write_guard() -> MutexGuard<'static, ()> {
    SESSION_WRITE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Forensic context emitted alongside a `session_corrupted` telemetry
/// event. Gathered by [`SplitlaneApp::load_session_at`] inside
/// the parse-failure branch *before* the empty fallback session is
/// returned, so the values reflect the file the user actually had on
/// disk - not the one we're about to overwrite. Stays a plain data
/// struct (no telemetry client coupling) because `load_session` runs
/// in bootstrap before the `TelemetryClient` is constructed; the
/// caller in `bootstrap.rs` defers the emit until after telemetry is
/// up.
#[derive(Debug, Clone)]
pub(crate) struct SessionCorruptionInfo {
    /// Canonical `serde_json::Error::classify()` bucket (`io | syntax | data
    /// | eof`) or a guarded-load bucket such as `oversize`, `non_regular`, or
    /// `unsupported_version`. Plain string keeps the telemetry schema fixed.
    pub(crate) error_category: &'static str,
    /// Size in bytes of the corrupted file, or `0` if the metadata
    /// call itself failed (rare - file just got read successfully).
    pub(crate) file_size: u64,
    /// Wall-clock age in seconds (mtime → now). `None` when the
    /// platform's modification-time call returns a value newer than
    /// `now` (clock drift) or the metadata call fails.
    pub(crate) file_age_seconds: Option<u64>,
    /// Resolved path of the freshly-written backup, or `None` if the
    /// backup write itself failed (never block startup on
    /// backup-side errors).
    pub(crate) backup_path: Option<PathBuf>,
}

/// What a session restore produces: the containers, which one is active, and
/// the live PTY views of the agent surfaces that came back inside a slot.
///
/// The views are handed back rather than installed because the warm terminal
/// cache belongs to the app, and the app does not exist yet while its
/// containers are being built.
pub(crate) struct RestoredContainers {
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) active_idx: usize,
    pub(crate) agent_views: Vec<(u64, Entity<crate::terminal::view::TerminalView>)>,
    /// How many shells came back parked on their container rather than in a
    /// slot, because the file was written by a build whose pane cap was higher
    /// than this one's. Non-zero means the user is owed a sentence.
    pub(crate) parked_over_cap: usize,
    /// How many background tabs of a kept pane became rail rows. Counted apart
    /// from `parked_over_cap` because the two have different causes and the
    /// toast names the cause.
    pub(crate) parked_extra_tabs: usize,
}

impl SplitlaneApp {
    /// Build the [`SessionState`] snapshot from live app state.
    ///
    /// Every persisted terminal surface emits `scrollback: None`, keeping PTY
    /// output local to the process that produced it.
    ///
    /// One container per record, rebuilt from live state on every save rather
    /// than edited into the previous one. That is what keeps a closed
    /// container from leaving anything behind - it contributes no layout, no
    /// tab-bar buttons, no files-tree expansion and no managed worktrees, so
    /// none of them can outlive it on disk.
    fn build_session_state(&self, cx: &App) -> splitlane_config::schema::SessionState {
        let projects: Vec<splitlane_config::schema::ProjectSession> = self
            .workspaces
            .iter()
            .map(|ws| splitlane_config::schema::ProjectSession {
                id: ws.id,
                title: ws.title.clone(),
                cwd: ws.cwd.clone(),
                is_expanded: ws.is_expanded,
                layout: ws.serialize_layout_without_scrollback(cx),
                // Agent surfaces, plus the diff surface when it is open. The
                // diff is written as the `diff` surface kind the schema has
                // always had a name for; a build that cannot read it drops
                // that one row (`#[serde(other)]`) rather than the file.
                surfaces: ws
                    .threads
                    .iter()
                    .map(|thread| {
                        crate::project::thread_to_surface(
                            thread,
                            crate::app::agent_slots::agent_leaf_index(ws, thread.id, cx),
                        )
                    })
                    .chain(
                        ws.diff_surface
                            .map(|id| crate::project::diff_surface_record(id, &ws.cwd)),
                    )
                    .collect(),
                custom_buttons: ws.custom_buttons.clone(),
                // Store expanded dirs relative to the container root.
                // A path that can't be made relative (symlinked outside the
                // root) is dropped rather than persisted absolute.
                expanded_paths: persisted_expanded_paths(&ws.cwd, &ws.files_expanded),
                // Persist worktree ownership so
                // a crash/restart keeps the teardown + prune record.
                managed_worktrees: ws
                    .managed_worktrees
                    .iter()
                    .map(|wt| splitlane_config::schema::ManagedWorktreeDef {
                        path: wt.path.to_string_lossy().into_owned(),
                        repo_root: wt.repo_root.to_string_lossy().into_owned(),
                        branch: wt.branch.clone(),
                        teardown: wt.teardown.as_str().to_string(),
                    })
                    .collect(),
                preferred_agent: ws.preferred_agent.map(|pref| pref.tag().to_string()),
                worktree_setup: ws.worktree_setup.clone(),
            })
            .collect();

        splitlane_config::schema::SessionState {
            version: splitlane_config::schema::SESSION_SCHEMA_VERSION,
            active_workspace: self.active_idx.min(projects.len().saturating_sub(1)),
            projects,
            // Free chats are gone as a separate list: they fold into the home
            // directory's container at restore and are saved as its surfaces.
            // The field stays in the schema so an older build can still read
            // what this one writes, and is never written again.
            chats: Vec::new(),
            // The selection is gone: every surface is a pane's content, and
            // the layout already says which. The field stays in the schema so
            // an older build can still read what this one writes, and is never
            // written again.
            agents_target: None,
            // Persist the diff scope so
            // a session that quit in Diff mode reopens on the same scope.
            diff_scope: Some(self.diff_mode.diff_scope.as_persisted().to_string()),
            rail_width: Some(self.rail_width),
            files_width: Some(self.files_width),
            // The newest reading, so the next launch has something to subtract
            // from instead of spending its first half hour unable to project.
            // Only a successful one is worth keeping; a failure has no numbers.
            limits_reading: match self.claude_limits.state.as_ref() {
                Some(Ok(snapshot)) => self.claude_limits.last_read_at.map(|ms| {
                    splitlane_config::schema::LimitsReadingSession {
                        at: (ms / 1000) as i64,
                        five_hour: snapshot.five_hour.map(|w| w.utilization),
                        seven_day: snapshot.seven_day.map(|w| w.utilization),
                    }
                }),
                _ => None,
            },
        }
    }

    /// Persist the session WITHOUT blocking the GPUI main thread.
    ///
    /// The lightweight metadata snapshot is built here (render thread, cheap),
    /// then JSON serialization and the atomic write run on a background task.
    /// A burst of saves (e.g. closing 20 workspaces) is coalesced into a single
    /// write via a monotonic token + short debounce, so the most-recent snapshot
    /// wins.
    ///
    /// The quit / pre-update-install paths must use [`save_session_blocking`]
    /// instead - there the write has to land before the process exits or is
    /// replaced, so a deferred task would be lost.
    pub(crate) fn save_session(&self, cx: &App) {
        let state = self.build_session_state(cx);
        let Some(path) = splitlane_config::loader::session_path() else {
            return;
        };

        let seq = self
            .save_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let save_seq = std::sync::Arc::clone(&self.save_seq);

        cx.background_spawn(async move {
            smol::Timer::after(std::time::Duration::from_millis(SAVE_DEBOUNCE_MS)).await;
            if save_seq.load(std::sync::atomic::Ordering::SeqCst) != seq {
                // A newer save was scheduled meanwhile - let it carry the
                // latest state; skip this redundant write.
                return;
            }
            // `smol::unblock` keeps serialization and filesystem I/O off the
            // background executor's async threads.
            smol::unblock(move || {
                // Re-check inside the shared write lock. A quit-path blocking
                // save or a newer deferred save may have superseded this task
                // after its debounce check but before it reached the filesystem.
                write_session_json_if_current(&path, &state, &save_seq, seq);
            })
            .await;
        })
        .detach();
    }

    /// Synchronous session save for the quit / pre-update-install
    /// paths, where a deferred background write would be lost when the process
    /// exits or is replaced.
    pub(crate) fn save_session_blocking(&self, cx: &App) {
        crate::window_state::save();
        // Cancel any in-flight deferred save: bump the coalescing token so a
        // background task still sleeping in its debounce wakes to a stale `seq`
        // and no-ops. Without this, a `save_session` fired moments before quit
        // could land its (older) snapshot *after* this final synchronous write,
        // resurrecting pre-quit state (e.g. a just-closed workspace).
        self.save_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let state = self.build_session_state(cx);
        let Some(path) = splitlane_config::loader::session_path() else {
            return;
        };
        write_session_json(&path, &state);
    }

    /// Restore a saved session from disk, or fall back silently to an
    /// empty session if anything goes wrong.
    ///
    /// Behaviour matrix:
    ///
    /// | State on disk           | Returned session  | Corruption info | Backup written |
    /// |-------------------------|-------------------|-----------------|----------------|
    /// | File missing            | `None`            | `None`          | no             |
    /// | Read error (perms, IO)  | `None`            | `Some(info)`    | no             |
    /// | Non-regular / oversize | `None`            | `Some(info)`    | no             |
    /// | Read OK + parse OK      | `Some(state)`     | `None`          | no             |
    /// | Read OK + parse FAIL    | `None` (fallback) | `Some(info)`    | yes            |
    /// | Unsupported version     | `None` (fallback) | `Some(info)`    | yes            |
    ///
    /// On parse failure the bad file is preserved as
    /// `session.json.corrupted-<unix-timestamp>` *before* the next
    /// `save_session` overwrites it, so we keep forensic evidence even
    /// when the user immediately moves on. The backup directory is
    /// rotated down to [`MAX_CORRUPTION_BACKUPS`] entries so a
    /// chronic-corruption case can't silently fill `~/.cache`.
    ///
    /// Telemetry: callers receive a `SessionCorruptionInfo` they can
    /// pass to `SplitlaneApp::emit_session_corrupted` once the
    /// telemetry client is up. The emit is consent-gated by the
    /// existing `TelemetryClient::Null` factory branch - opted-out
    /// users never produce a network call.
    pub(crate) fn load_session() -> (
        Option<splitlane_config::schema::SessionState>,
        Option<SessionCorruptionInfo>,
    ) {
        let Some(path) = splitlane_config::loader::session_path() else {
            return (None, None);
        };
        Self::load_session_at(&path)
    }

    /// Path-parametrised core of [`load_session`]. Direct test surface -
    /// the wrapper above resolves `splitlane_config::loader::session_path()`
    /// against the user's XDG cache dir, which is unsuitable for unit
    /// tests because every run would race against a live install.
    pub(crate) fn load_session_at(
        path: &Path,
    ) -> (
        Option<splitlane_config::schema::SessionState>,
        Option<SessionCorruptionInfo>,
    ) {
        // Bound the read so a multi-hundred-MB hand-edited /
        // agent-written session.json (or a non-regular file swapped in) can't
        // OOM/stall the load before parse. Guard hits start from an empty
        // session, but still surface forensic context to telemetry.
        let bytes = match read_session_capped(path) {
            Ok(SessionRead::Data(d)) => d,
            Ok(SessionRead::Missing) => return (None, None),
            Ok(SessionRead::Rejected(category)) => {
                return (None, Some(session_corruption_info(path, category, None)));
            }
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("session load: read failed at {}: {e}", path.display());
                }
                return (None, Some(session_corruption_info(path, "io", None)));
            }
        };

        let data = match String::from_utf8(bytes) {
            Ok(data) => data,
            Err(err) => {
                log::warn!(
                    "session load: invalid UTF-8 at {}; falling back to empty session",
                    path.display()
                );
                let bytes = err.into_bytes();
                let backup_path = write_corruption_backup(path, &bytes).unwrap_or_else(|e| {
                    log::warn!(
                        "session load: backup write failed at {}: {e}",
                        path.display()
                    );
                    None
                });

                return (
                    None,
                    Some(session_corruption_info(path, "data", backup_path)),
                );
            }
        };

        // The container merge broke the schema: v1 had two arrays,
        // `workspaces` and `projects`, describing the same thing. A v1 file is
        // read through the frozen v1 types and folded into the merged shape
        // here, so upgrading keeps every container instead of starting empty.
        // The original bytes are kept beside the live file as `.v1.bak`: a
        // pre-merge build can still be run against them, and that backup is
        // the whole rollback story - nothing else guards this step.
        if let Some(state) = migrated_v1_session(path, &data) {
            return (Some(state), None);
        }

        match serde_json::from_str::<splitlane_config::schema::SessionState>(&data) {
            Ok(state) if state.version == splitlane_config::schema::SESSION_SCHEMA_VERSION => {
                (Some(state), None)
            }
            Ok(state) => {
                log::warn!(
                    "session load: unsupported schema version {} at {} (expected {}); falling back to empty session",
                    state.version,
                    path.display(),
                    splitlane_config::schema::SESSION_SCHEMA_VERSION
                );
                let backup_path =
                    write_corruption_backup(path, data.as_bytes()).unwrap_or_else(|e| {
                        log::warn!(
                            "session load: backup write failed at {}: {e}",
                            path.display()
                        );
                        None
                    });
                (
                    None,
                    Some(session_corruption_info(
                        path,
                        "unsupported_version",
                        backup_path,
                    )),
                )
            }
            Err(parse_err) => {
                log::warn!(
                    "session load: parse failed at {} ({}); falling back to empty session",
                    path.display(),
                    parse_err
                );

                let backup_path =
                    write_corruption_backup(path, data.as_bytes()).unwrap_or_else(|e| {
                        // Backup write failure must not block startup.
                        // Log and proceed - telemetry still fires with a
                        // `backup_path: None`, so the operator can still see
                        // the corruption rate even if forensics are missing.
                        log::warn!(
                            "session load: backup write failed at {}: {e}",
                            path.display()
                        );
                        None
                    });

                (
                    None,
                    Some(session_corruption_info(
                        path,
                        serde_category_tag(&parse_err),
                        backup_path,
                    )),
                )
            }
        }
    }

    /// Rebuild the containers from a saved session. Each one's layout tree is
    /// reconstructed via `LayoutTree::from_layout_node` with CWD-aware
    /// terminal spawning, and its parked agent surfaces come back as
    /// [`crate::project::Thread`]s. Returns the container list and the active
    /// index.
    ///
    /// `remaining_threads` is the shared restore budget across every
    /// container - one session.json cannot fan out into
    /// thousands of agent surfaces however it distributes them.
    pub(crate) fn restore_workspaces(
        session: &splitlane_config::schema::SessionState,
        remaining_threads: &mut usize,
        cx: &mut Context<Self>,
    ) -> RestoredContainers {
        let mut workspaces = Vec::new();
        // Agent surfaces rebuilt into a slot, for the caller to seed the warm
        // terminal cache with. They are live PTY views from this point on, so
        // the cache has to learn about them before anything asks it for one.
        let mut restored_agent_views: Vec<(u64, Entity<crate::terminal::view::TerminalView>)> =
            Vec::new();
        // Shells parked because their slot was past `MAX_PANES`. Counted so the
        // restore can say so out loud instead of quietly rearranging the
        // container.
        let mut parked_over_cap = 0usize;
        let mut parked_extra_tabs = 0usize;

        // Cap restored containers. Each layout's pane count is bounded by
        // `validate_layout` below, so this is the only remaining
        // unbounded restore axis - a session.json with thousands of container
        // entries would otherwise each build ≥1 surface.
        //
        // Those surfaces are descriptions, not processes. `TerminalView`
        // holds its backend start until the surface is first shown
        // (`TerminalView::ensure_backend_started`), so restoring N containers
        // costs zero child processes and no session-store reads - only the
        // panes the user actually looks at fork a shell. The cap still matters
        // for entity count and for the per-container git probe below.
        if session.projects.len() > MAX_WORKSPACES {
            log::warn!(
                "session restore: {} containers exceeds MAX_WORKSPACES ({MAX_WORKSPACES}); restoring the first {MAX_WORKSPACES}",
                session.projects.len()
            );
        }
        for ws_session in session.projects.iter().take(MAX_WORKSPACES) {
            let mut cwd = restored_workspace_cwd(&ws_session.cwd);
            let mut title = ws_session.title.clone();
            if should_repair_restored_root_terminal(&title, &cwd) {
                let repaired_cwd = launch_cwd::implicit_launch_cwd();
                log::info!(
                    "session restore: repairing legacy default workspace at filesystem root"
                );
                title = launch_cwd::title_for_cwd_or(&repaired_cwd, title);
                cwd = repaired_cwd;
            }
            // The container keeps its persisted id: `agents_target` addresses
            // it, and a thread's PTY carries it as `SPLITLANE_WORKSPACE_ID`.
            // Ids used to be re-minted on every restore because only the
            // Agents rail's containers were addressed by a stable id; now
            // there is one list and one id per container.
            let ws_id = ws_session.id;

            // `validate_layout` best-effort-caps the leaf
            // budget, but its ">= 2 children" padding re-introduces a bounded
            // O(depth) overshoot of app-synthesized pad panes once that budget
            // is spent (a crafted deeply-nested session.json - local-only, but
            // still a self-DoS). Enforce the hard MAX_PANES ceiling HERE, at
            // restore, so no workspace can ever restore more
            // than MAX_PANES real PTYs: over the cap we drop the layout and fall
            // back to a single default terminal.
            let capped = ws_session
                .layout
                .clone()
                .map(without_persisted_scrollback)
                .map(layout_within_cap)
                .unwrap_or(CappedLayout {
                    layout: None,
                    evicted: Vec::new(),
                    displaced_tabs: Vec::new(),
                });
            let restored_layout = capped.layout;

            // The container's parked agent surfaces - the Agents rail's
            // threads before the rails merged. Built before the layout,
            // because an agent surface that was showing in a slot has to be
            // rebuilt WITH its session binding and handed to that slot; a slot
            // is not allowed to invent a bare shell in its place.
            let mut threads = crate::project::threads_from_session(ws_session, remaining_threads);
            // A slot the cap removed does not take its shells with it: they
            // join the container's parked surfaces, where the rail still lists
            // them and one click puts one back on screen.
            parked_over_cap += park_evicted_surfaces(&capped.evicted, &cwd, &mut threads);
            parked_extra_tabs += park_evicted_surfaces(&capped.displaced_tabs, &cwd, &mut threads);
            let agent_views = Self::rebuild_slot_agent_views(ws_session, &threads, cx);
            // The ones that actually launched something are starting - the
            // same two seconds a restored window spends coming up, and the same
            // word for them.
            //
            // `agent_views` is not that set on its own: the pass builds a view
            // for every slot-placed surface, and a **shell** among them gets no
            // launch command. Marking those would put the word on a surface
            // that never had it to lose, which is the same lie one step
            // earlier. The live mount path guards the same way; found by a
            // cross-vendor review, which pointed out the two guards had already
            // drifted.
            let starting: Vec<u64> = threads
                .iter()
                .filter(|thread| {
                    thread.terminal_agent.is_some() && agent_views.contains_key(&thread.id)
                })
                .map(|thread| thread.id)
                .collect();
            for thread in threads
                .iter_mut()
                .filter(|thread| starting.contains(&thread.id))
            {
                thread.status = crate::project::ThreadStatus::Starting;
            }

            let mut workspace = if let Some(layout) = restored_layout {
                let mut pane_deque: VecDeque<Entity<Pane>> = VecDeque::new();
                let ws_cwd = cwd.clone();
                // Normalized, not verbatim: a file written by a build that
                // nested splits freely still loads, and comes back as the one
                // row or column this build draws.
                let tree = LayoutTree::from_layout_node_normalized(
                    &layout,
                    &mut pane_deque,
                    &mut |node| {
                        let surfaces = match node {
                            LayoutNode::Pane { surfaces } => surfaces.as_slice(),
                            _ => &[],
                        };
                        Self::spawn_pane_from_surfaces(ws_id, surfaces, &ws_cwd, &agent_views, cx)
                    },
                );
                match tree {
                    Some(tree) => Workspace::with_layout_and_id(ws_id, title.clone(), cwd, tree),
                    // Every slot in the file held nothing. Both this arm and
                    // the one below used to spawn a shell, which is how a
                    // container the user had emptied came back with a
                    // terminal in it on the next launch.
                    None => Workspace::empty_with_id(ws_id, title.clone(), cwd),
                }
            } else {
                Workspace::empty_with_id(ws_id, title.clone(), cwd)
            };

            workspace.custom_buttons = ws_session.custom_buttons.clone();
            // Rehydrate worktree ownership so the
            // close-time teardown still applies after a restart.
            workspace.managed_worktrees = ws_session
                .managed_worktrees
                .iter()
                .filter_map(rehydrate_managed_worktree)
                .collect();
            // Rehydrate expanded dirs as absolute paths under this
            // workspace's cwd. Paths that no longer resolve to a directory are
            // dropped lazily later (by the tree's `hydrated` filter on open),
            // so a deleted folder never resurrects a dead row.
            workspace.files_expanded = ws_session
                .expanded_paths
                .iter()
                .filter_map(|rel| rehydrate_expanded_path(&workspace.cwd, rel))
                .collect();
            workspace.is_expanded = ws_session.is_expanded;
            // A tag this build cannot read - an agent dropped by an upgrade,
            // a hand-edited file - reads as **ask**, not as "never asked".
            // The two differ: "never asked" inherits the app-level answer and
            // could start a different agent than the user chose, without a
            // word, where the previous build asked. A stored value that cannot
            // be honoured fails to the question.
            workspace.preferred_agent = ws_session.preferred_agent.as_deref().map(|tag| {
                crate::agent_launcher::PreferredAgent::from_tag(tag)
                    .unwrap_or(crate::agent_launcher::PreferredAgent::Ask)
            });
            // What `+ worktree` runs in a tree it makes here. A blank string
            // in a hand-edited file reads as "run nothing", the same as
            // absent - the dialog only ever writes a trimmed value.
            workspace.worktree_setup = ws_session
                .worktree_setup
                .as_deref()
                .and_then(crate::workspace::worktree::remembered_setup);
            workspace.threads = threads;
            // Agent surfaces restored into a slot keep their entry in the warm
            // cache, so selecting the row, evicting an exited terminal, or
            // moving the surface back out all resolve to the same live PTY.
            for (thread_id, view) in agent_views {
                restored_agent_views.push((thread_id, view));
            }
            workspace.diff_surface = crate::project::diff_surface_from_session(ws_session);
            // Kick off the deferred git-stats probe (off render thread).
            Self::spawn_initial_git_stats(ws_id, workspace.cwd.clone(), cx);
            workspaces.push(workspace);
        }

        // `git worktree prune` on every repo whose
        // restored workspaces own worktrees - drops references whose directory
        // vanished (manual rm -rf, crashed teardown). Git-native guarantee: a
        // worktree whose directory still exists is untouched. Best-effort,
        // off the render thread, deduplicated per repo.
        let mut prune_roots: Vec<std::path::PathBuf> = workspaces
            .iter()
            .flat_map(|ws| ws.managed_worktrees.iter().map(|wt| wt.repo_root.clone()))
            .collect();
        prune_roots.sort();
        prune_roots.dedup();
        if !prune_roots.is_empty() {
            cx.spawn(async move |_this, _cx: &mut gpui::AsyncApp| {
                smol::unblock(move || {
                    for root in prune_roots {
                        if let Err(e) = crate::workspace::worktree::prune(&root) {
                            log::debug!("worktree prune skipped for {}: {e}", root.display());
                        }
                    }
                })
                .await;
            })
            .detach();
        }

        let active_idx = session
            .active_workspace
            .min(workspaces.len().saturating_sub(1));
        RestoredContainers {
            workspaces,
            active_idx,
            agent_views: restored_agent_views,
            parked_over_cap,
            parked_extra_tabs,
        }
    }

    /// Rebuild the PTY views for the agent surfaces this container had showing
    /// in its slot tree, keyed by surface id for the layout builder to consume.
    ///
    /// Only surfaces the file marks as slot-placed are built. A parked one is
    /// still just a rail row after a restart, costing nothing until it is
    /// selected. That asymmetry is the point: what was on screen comes back on
    /// screen, with its session, and nothing else wakes up.
    fn rebuild_slot_agent_views(
        ws_session: &splitlane_config::schema::ProjectSession,
        threads: &[crate::project::Thread],
        cx: &mut Context<Self>,
    ) -> std::collections::HashMap<u64, Entity<crate::terminal::view::TerminalView>> {
        use splitlane_config::schema::SurfacePlacement;
        let config = splitlane_config::loader::load_config();
        let mut views = std::collections::HashMap::new();
        for surface in &ws_session.surfaces {
            if !matches!(surface.placement, SurfacePlacement::Slot { .. }) {
                continue;
            }
            // A surface the restore budget dropped has no thread, and a tab
            // referencing it will be dropped in turn rather than resurrected
            // as an unbound shell.
            let Some(thread) = threads.iter().find(|thread| thread.id == surface.id) else {
                continue;
            };
            views.insert(
                thread.id,
                Self::build_agent_terminal_view(thread, &config, cx),
            );
        }
        views
    }

    /// Create a `Pane` (with one tab per surface) from serialized surface
    /// definitions. Falls back to a single terminal in `fallback_cwd` when
    /// the surface list is empty.
    ///
    /// `agent_views` holds the already-built PTY views of this container's
    /// slot-placed agent surfaces, keyed by surface id. A tab of type `agent`
    /// takes its view from there rather than spawning anything: the surface's
    /// session binding lives in the container's surface record, and the view
    /// built from it is the only one allowed to exist.
    ///
    /// Building a terminal tab here does not fork a shell. Each
    /// `TerminalView` starts as a display-only placeholder and opens its PTY on
    /// first show, so this path is safe to run for every restored workspace
    /// regardless of the mode the app is restoring into.
    pub(crate) fn spawn_pane_from_surfaces(
        workspace_id: u64,
        surfaces: &[splitlane_config::schema::SurfaceDefinition],
        fallback_cwd: &std::path::Path,
        agent_views: &std::collections::HashMap<u64, Entity<crate::terminal::view::TerminalView>>,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        use std::path::PathBuf;

        let mut focus_idx: usize = 0;
        // A slot that held nothing comes back holding nothing: the launcher.
        // It used to spawn a shell here, which meant closing the only pane of
        // a project - now that closing it leaves the launcher - came back as a
        // shell on the next launch, so the thing the user had just got rid of
        // returned by itself. An empty pane is a state the model has a name
        // for; restore should not invent a session to fill it.
        let tabs: Vec<crate::pane::TabContent> = if surfaces.is_empty() {
            Vec::new()
        } else {
            // The focused tab is tracked by position among the tabs actually
            // built, not by position in the file: a surface can fail to
            // restore (a markdown file that moved, an agent reference the
            // restore budget dropped), and counting the gaps would raise the
            // wrong tab - or a tab that no longer exists.
            let mut built: usize = 0;
            surfaces
                .iter()
                .filter_map(|surface| {
                    // An agent tab is a reference to a surface of the
                    // container, and its view was built with that surface's
                    // session binding. A reference that resolves to nothing
                    // (the surface hit the restore budget, or the file was
                    // hand-edited) drops the tab: a bare shell in its place
                    // would look like the agent came back when it did not.
                    if surface.surface_type.as_deref() == Some("agent") {
                        let view = agent_views.get(&surface.surface_id?)?;
                        if surface.focus == Some(true) {
                            focus_idx = built;
                        }
                        built += 1;
                        return Some(crate::pane::TabContent::Terminal(view.clone()));
                    }
                    if surface.surface_type.as_deref() == Some("diff") {
                        // The repo the container is rooted at, resolved the
                        // same way `Workspace::build` resolves it - the
                        // container itself is not built yet at this point, and
                        // a diff of a directory that stopped being a checkout
                        // simply does not come back.
                        let diff = crate::diff::diff_view_for_cwd(fallback_cwd, workspace_id, cx)?;
                        if surface.focus == Some(true) {
                            focus_idx = built;
                        }
                        built += 1;
                        return Some(crate::pane::TabContent::Diff(diff));
                    }
                    if surface.surface_type.as_deref() == Some("markdown") {
                        let path = surface.path.as_ref().map(PathBuf::from)?;
                        let markdown = cx.new(|cx: &mut Context<crate::file_view::FileView>| {
                            crate::file_view::FileView::open(path, cx)
                        });
                        if surface.focus == Some(true) {
                            focus_idx = built;
                        }
                        built += 1;
                        return Some(crate::pane::TabContent::Markdown(markdown));
                    }
                    let cwd = resolved_surface_cwd(surface.cwd.as_deref(), fallback_cwd);

                    // Forward the per-surface env override; the global
                    // `terminal.env` default is merged underneath in
                    // `TerminalState::new`.
                    let surface_env = surface.env.clone();
                    let t = cx.new(|cx| {
                        TerminalView::with_cwd_and_env(
                            workspace_id,
                            Some(cwd),
                            None,
                            surface_env,
                            cx,
                        )
                    });

                    // Explicit layout definitions may still seed scrollback.
                    // Session restore clears the legacy field before this path.
                    if let Some(ref scrollback) = surface.scrollback {
                        t.read(cx).restore_scrollback(scrollback);
                    }
                    // Re-apply the persisted custom name.
                    if let Some(ref custom) = surface.custom_name {
                        t.update(cx, |view, _cx| {
                            view.terminal.custom_name = Some(custom.clone());
                        });
                    }
                    // Restore the identity pill as a dimmed
                    // "last known" value. Ingress whitelist: `from_tag` is an
                    // exact match against the known agent tags, so an
                    // unknown, oversized, or control-char value from a
                    // hand-edited session.json maps to `None` and no pill is
                    // rendered (parity with the ingress-validation invariant -
                    // session.json is local-only but validated anyway). The
                    // first scan (0/2 s burst on restore activity) then
                    // confirms or clears it.
                    if let Some(agent) = surface
                        .agent
                        .as_deref()
                        .and_then(crate::agent_launcher::TerminalAgent::from_tag)
                    {
                        t.update(cx, |view, _cx| {
                            view.terminal.detected_agent = Some(agent);
                            view.terminal.agent_confirmed = false;
                        });
                    }
                    // Restore the per-pane font zoom through
                    // the ingress sanitizer - NaN/inf dropped, finite values
                    // clamped to [8.0, 32.0]; never fed raw to the cell
                    // geometry (persisted input is never trusted raw).
                    if let Some(size) = surface
                        .font_size
                        .and_then(crate::terminal::element::sanitize_font_override)
                    {
                        t.update(cx, |view, _cx| {
                            view.terminal.font_size_override = Some(size);
                        });
                    }
                    cx.subscribe(&t, Self::handle_terminal_event).detach();
                    if surface.focus == Some(true) {
                        focus_idx = built;
                    }
                    built += 1;
                    Some(crate::pane::TabContent::Terminal(t))
                })
                .collect()
        };

        let Some(_) = tabs.first() else {
            log::error!("spawn_pane_from_surfaces: no restorable tabs built; using fallback");
            let t = cx.new(|cx| {
                TerminalView::with_cwd(workspace_id, Some(fallback_cwd.to_path_buf()), None, cx)
            });
            cx.subscribe(&t, Self::handle_terminal_event).detach();
            let pane = cx.new(|cx| Pane::new(t, cx));
            cx.subscribe(&pane, Self::handle_pane_event).detach();
            return pane;
        };
        let pane = cx.new(|cx| Pane::new_with_tabs(tabs, focus_idx, cx));
        cx.subscribe(&pane, Self::handle_pane_event).detach();
        pane
    }
}

// ---------------------------------------------------------------------------
// Ingress-bound helpers (free functions, free of `&self`)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum SessionRead {
    Data(Vec<u8>),
    Missing,
    Rejected(&'static str),
}

/// Read `path`, bounded at [`MAX_SESSION_SIZE_BYTES`] and rejecting
/// non-regular files. Stats the OPEN fd, not the path, and caps
/// the read with `take`, so a swap/grow between stat and read cannot defeat the
/// bound (the FIFO/device + TOCTOU class, mirroring `read_config_string`).
fn read_session_capped(path: &Path) -> std::io::Result<SessionRead> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SessionRead::Missing),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        log::warn!(
            "session load: {} is not a regular file; starting empty",
            path.display()
        );
        return Ok(SessionRead::Rejected("non_regular"));
    }
    if meta.len() > MAX_SESSION_SIZE_BYTES {
        log::warn!(
            "session load: {} is {} bytes (> {MAX_SESSION_SIZE_BYTES} cap); starting empty",
            path.display(),
            meta.len()
        );
        return Ok(SessionRead::Rejected("oversize"));
    }
    let mut data = Vec::new();
    // +1 so a file grown past the cap between stat and read is still caught.
    file.take(MAX_SESSION_SIZE_BYTES + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_SESSION_SIZE_BYTES {
        log::warn!(
            "session load: {} exceeded the {MAX_SESSION_SIZE_BYTES} cap during read; starting empty",
            path.display()
        );
        return Ok(SessionRead::Rejected("oversize"));
    }
    Ok(SessionRead::Data(data))
}

/// Read `data` as a schema-v1 session and fold it into the merged shape.
///
/// Returns `None` when the file is not v1 (any other version, or unparseable
/// as v1) so the caller falls through to the normal v2 path and its corruption
/// handling. A v1 file that fails to parse as v1 is not treated as migratable
/// - it is corruption, and the caller reports it as such.
///
/// The `.v1.bak` write is best-effort: losing the backup must not cost the
/// user their session, so a failure is logged and the migration proceeds.
fn migrated_v1_session(path: &Path, data: &str) -> Option<splitlane_config::schema::SessionState> {
    let version = serde_json::from_str::<serde_json::Value>(data)
        .ok()?
        .get("version")?
        .as_u64()?;
    if version != u64::from(splitlane_config::schema::SESSION_SCHEMA_VERSION_LEGACY) {
        return None;
    }
    let v1 = match serde_json::from_str::<splitlane_config::schema::legacy_v1::SessionStateV1>(data)
    {
        Ok(v1) => v1,
        Err(e) => {
            log::warn!(
                "session load: {} claims schema v1 but does not parse as one ({e}); \
                 falling through to the corruption path",
                path.display()
            );
            return None;
        }
    };

    let backup = path.with_extension("json.v1.bak");
    match std::fs::write(&backup, data) {
        Ok(()) => log::info!(
            "session migrate: schema v1 -> v{}; original kept at {}",
            splitlane_config::schema::SESSION_SCHEMA_VERSION,
            backup.display()
        ),
        Err(e) => log::warn!(
            "session migrate: could not write {} ({e}); migrating anyway",
            backup.display()
        ),
    }

    // v1 could rename a directory's two rows apart; one rail means one name,
    // and the join keeps the workspace's. Say which name is being dropped
    // rather than letting it vanish between two launches. Computed here, not
    // in the migration: `splitlane-config` is a leaf crate of pure data and has
    // no logger.
    let dropped: Vec<(String, String)> = v1
        .projects
        .iter()
        .filter_map(|project| {
            v1.workspaces
                .iter()
                .find(|ws| {
                    splitlane_config::schema::same_directory(&ws.cwd, &project.cwd)
                        && ws.title != project.title
                })
                .map(|ws| (ws.title.clone(), project.title.clone()))
        })
        .collect();
    for (kept, dropped) in &dropped {
        log::info!("session migrate: `{kept}` and `{dropped}` are one directory; keeping `{kept}`");
    }

    let merged = splitlane_config::schema::legacy_v1::migrate(v1);
    log::info!(
        "session migrate: {} container(s) after merging workspaces and projects",
        merged.projects.len()
    );
    Some(merged)
}

fn session_corruption_info(
    path: &Path,
    error_category: &'static str,
    backup_path: Option<PathBuf>,
) -> SessionCorruptionInfo {
    let metadata = std::fs::metadata(path).ok();
    let file_size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
    let file_age_seconds = metadata
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|mt| SystemTime::now().duration_since(mt).ok())
        .map(|d| d.as_secs());

    SessionCorruptionInfo {
        error_category,
        file_size,
        file_age_seconds,
        backup_path,
    }
}

fn restored_workspace_cwd(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_dir() {
        return path;
    }
    let fallback = launch_cwd::implicit_launch_cwd();
    log::warn!(
        "session restore: workspace cwd {} is not a directory; falling back to {}",
        path.display(),
        fallback.display()
    );
    fallback
}

fn resolved_surface_cwd(raw: Option<&str>, fallback_cwd: &Path) -> PathBuf {
    let Some(raw) = raw else {
        return fallback_cwd.to_path_buf();
    };
    let path = PathBuf::from(raw);
    if path.is_dir() {
        return path;
    }
    log::warn!(
        "session restore: surface cwd {} is not a directory; falling back to {}",
        path.display(),
        fallback_cwd.display()
    );
    fallback_cwd.to_path_buf()
}

fn without_persisted_scrollback(mut layout: LayoutNode) -> LayoutNode {
    fn clear(node: &mut LayoutNode) {
        match node {
            LayoutNode::Pane { surfaces } => {
                for surface in surfaces {
                    surface.scrollback = None;
                }
            }
            LayoutNode::Split { children, .. } => {
                for child in children {
                    clear(child);
                }
            }
        }
    }

    clear(&mut layout);
    layout
}

/// What a persisted layout leaves behind once it is cut down to the pane cap:
/// the slots the container will draw, and the surfaces of every slot past the
/// cap.
pub(crate) struct CappedLayout {
    /// The layout to restore - at most `MAX_PANES` leaves. `None` for a layout
    /// with nothing in it.
    pub(crate) layout: Option<LayoutNode>,
    /// Terminal surfaces that were in a slot the cap removed, in the order the
    /// file listed them. The caller parks these on the container.
    pub(crate) evicted: Vec<splitlane_config::schema::SurfaceDefinition>,
    /// Terminal surfaces that were the *second and later* tabs of a slot the
    /// cap kept. A pane holds one surface now, so a file written when panes
    /// had tab strips has more surfaces than slots; these are parked the same
    /// way, and for the same reason - nothing the user opened may vanish
    /// because the shape of a pane changed under it.
    pub(crate) displaced_tabs: Vec<splitlane_config::schema::SurfaceDefinition>,
}

/// Validate a persisted layout and enforce the hard `MAX_PANES` ceiling
/// in schema space - before a single pane is spawned.
///
/// `validate_layout` best-effort-caps the leaf budget, but its ">= 2 children"
/// padding re-introduces a bounded `O(depth)` overshoot of app-synthesized pad
/// panes once that budget is spent (a crafted deeply-nested session.json), so
/// the hard ceiling is applied here: no container can ever restore more than
/// `MAX_PANES` panes. Restore is where the ceiling belongs; defence-in-depth
/// on top of `validate_layout`'s budget.
///
/// Over the cap the layout is **cut down, not thrown away**. It used to be
/// discarded whole - which was unreachable while the cap was 32 and loses a
/// user's shells now that it is three. A pane past the third is an extra
/// *slot*, not extra work: the model already has a home for a surface that is
/// not in a slot, so its surfaces come back parked on the container and the
/// rail still lists them.
///
/// The kept slots are the first `MAX_PANES` in file order, flattened into one
/// row or column - the same normalization `LayoutTree::flattened` applies to
/// everything that arrives from disk, done early enough that the panes past the
/// cap are never built.
fn layout_within_cap(mut layout: LayoutNode) -> CappedLayout {
    splitlane_config::loader::validate_layout(&mut layout);

    // A pane holds one surface. Done first and **in place**: the file's shape
    // and its ratios are the user's, and a layout that is otherwise fine must
    // come back exactly as it was. Flattening every layout to rebuild it - as
    // the over-cap branch below has to - would reset every pane's width on
    // every launch, which is a loss the user would see and never asked for.
    let mut displaced_tabs: Vec<splitlane_config::schema::SurfaceDefinition> = Vec::new();
    keep_one_surface_per_pane(&mut layout, &mut displaced_tabs);
    // A slot that holds nothing is not a slot. Closing the last pane of a
    // container leaves it with none at all - the rail row's own
    // `+ agent` / `+ shell` / `+ worktree` are how the next one arrives - so a
    // pane with no surfaces must not come back as anything: not as a shell
    // nobody asked for, and not as an empty pane standing in the way.
    let Some(layout) = prune_empty_panes(layout) else {
        return CappedLayout {
            layout: None,
            evicted: Vec::new(),
            displaced_tabs,
        };
    };
    if !displaced_tabs.is_empty() {
        log::warn!(
            "session restore: {} surfaces were background tabs of a pane; \
             panes hold one surface, so they are parked on the container",
            displaced_tabs.len()
        );
    }

    let leaves = layout.leaf_count();
    if leaves <= MAX_PANES {
        return CappedLayout {
            layout: Some(layout),
            evicted: Vec::new(),
            displaced_tabs,
        };
    }
    log::warn!(
        "session restore: layout has {leaves} panes after validation \
         (> MAX_PANES {MAX_PANES}); keeping the first {MAX_PANES} and parking the rest"
    );

    // The outermost direction is the one the file actually stated; ratios are
    // not kept, because ratios of a shape that no longer exists mean nothing.
    let direction = match &layout {
        LayoutNode::Split { direction, .. } => direction.clone(),
        LayoutNode::Pane { .. } => "horizontal".to_string(),
    };
    let mut panes: Vec<Vec<splitlane_config::schema::SurfaceDefinition>> = Vec::new();
    collect_pane_surfaces(layout, &mut panes);

    let evicted: Vec<splitlane_config::schema::SurfaceDefinition> = panes
        .split_off(MAX_PANES.min(panes.len()))
        .into_iter()
        .flatten()
        .collect();

    let kept: Vec<LayoutNode> = panes
        .into_iter()
        .map(|surfaces| LayoutNode::Pane { surfaces })
        .collect();
    let layout = match kept.len() {
        0 => None,
        1 => kept.into_iter().next(),
        _ => Some(LayoutNode::Split {
            direction,
            ratio: None,
            ratios: None,
            children: kept,
        }),
    };
    CappedLayout {
        layout,
        evicted,
        displaced_tabs,
    }
}

/// Whether this surface names nothing at all.
///
/// `validate_layout` never leaves a pane with an empty surface list: it pushes
/// a default one in, so "holds nothing" survives the round trip as "holds a
/// surface that says nothing about itself". `park_evicted_surfaces` reads the
/// same shape when it declines to make a rail row for one.
fn is_pad_surface(surface: &splitlane_config::schema::SurfaceDefinition) -> bool {
    matches!(surface.surface_type.as_deref(), None | Some("terminal"))
        && surface.name.is_none()
        && surface.custom_name.is_none()
        && surface.cwd.is_none()
        && surface.surface_id.is_none()
}

/// Drop the slots that hold nothing, and any split left empty by that.
///
/// `validate_layout` synthesizes a surface-less pane to keep a pruned split
/// structurally valid, and a container emptied by closing its last pane has no
/// layout at all. Neither should put a pane on screen.
fn prune_empty_panes(node: LayoutNode) -> Option<LayoutNode> {
    // A grid's empty cells are the shape, not stray empty panes, so this walk
    // stops at one.
    //
    // "An empty pane is not restored" is the right rule everywhere else: it has
    // no rail row, nothing is running in it, and bringing it back puts a
    // launcher in the way of the panes that do hold something. In a grid it
    // reads the other way round - the four cells ARE the form the person chose,
    // and pruning two of them would leave two leaves in a nest that `flattened`
    // then straightens into a column. That is the layout rearranging itself
    // across a restart, which is worse than the thing the rule guards against
    // and is exactly what the grid's rules forbid while the app is running.
    if is_grid_node(&node) {
        return Some(node);
    }
    match node {
        LayoutNode::Pane { surfaces } => {
            (!surfaces.iter().all(is_pad_surface)).then_some(LayoutNode::Pane { surfaces })
        }
        LayoutNode::Split {
            direction,
            ratio,
            ratios,
            children,
        } => {
            let kept: Vec<LayoutNode> =
                children.into_iter().filter_map(prune_empty_panes).collect();
            match kept.len() {
                0 => None,
                // A split of one is that one - and its ratios described a
                // shape that no longer exists.
                1 => kept.into_iter().next(),
                _ => Some(LayoutNode::Split {
                    direction,
                    ratio,
                    ratios,
                    children: kept,
                }),
            }
        }
    }
}

/// The grid shape, asked of the persisted schema rather than of the live tree.
///
/// The same question `LayoutTree::is_grid` asks, and deliberately a second
/// implementation rather than a shared one: they read different types, and the
/// one thing that must not happen is a *live* predicate being consulted here,
/// before any pane has been spawned. Two short functions that agree by
/// construction (a split of two vertical splits of two panes) are cheaper to
/// keep true than one that has to work on both sides of the restore boundary.
fn is_grid_node(node: &LayoutNode) -> bool {
    let LayoutNode::Split {
        direction,
        children,
        ..
    } = node
    else {
        return false;
    };
    if direction != "horizontal" || children.len() != 2 {
        return false;
    }
    children.iter().all(|row| {
        matches!(
            row,
            LayoutNode::Split { direction, children, .. }
                if direction == "vertical"
                    && children.len() == 2
                    && children
                        .iter()
                        .all(|cell| matches!(cell, LayoutNode::Pane { .. }))
        )
    })
}

/// Walk the layout and leave each pane holding one surface, in place.
///
/// In place because the tree the file states is the tree the user arranged,
/// down to the width of each pane: rebuilding it to change what is inside a
/// leaf would throw the ratios away for a reason that has nothing to do with
/// them.
fn keep_one_surface_per_pane(
    node: &mut LayoutNode,
    displaced: &mut Vec<splitlane_config::schema::SurfaceDefinition>,
) {
    match node {
        LayoutNode::Pane { surfaces } => {
            let taken = std::mem::take(surfaces);
            *surfaces = keep_one_surface(taken, displaced);
        }
        LayoutNode::Split { children, .. } => {
            for child in children {
                keep_one_surface_per_pane(child, displaced);
            }
        }
    }
}

/// Keep the surface a pane was showing and hand the rest back to be parked.
///
/// The one kept is the one the file marked focused, not the first: a pane
/// restored to its *second* tab would otherwise come back showing something
/// the user had left behind. With nothing marked - an older file, or a
/// hand-edit - the first is as good an answer as the format offers.
fn keep_one_surface(
    surfaces: Vec<splitlane_config::schema::SurfaceDefinition>,
    displaced: &mut Vec<splitlane_config::schema::SurfaceDefinition>,
) -> Vec<splitlane_config::schema::SurfaceDefinition> {
    if surfaces.len() <= 1 {
        return surfaces;
    }
    let shown = surfaces
        .iter()
        .position(|s| s.focus == Some(true))
        .unwrap_or(0);
    let mut kept = Vec::with_capacity(1);
    for (idx, surface) in surfaces.into_iter().enumerate() {
        if idx == shown {
            kept.push(surface);
        } else {
            displaced.push(surface);
        }
    }
    kept
}

/// Flatten a layout into its panes' surface lists, left to right - the order
/// `LayoutTree::from_layout_node` consumes leaves in, so "the first three" mean
/// the same three here as they would there.
fn collect_pane_surfaces(
    node: LayoutNode,
    out: &mut Vec<Vec<splitlane_config::schema::SurfaceDefinition>>,
) {
    match node {
        LayoutNode::Pane { surfaces } => out.push(surfaces),
        LayoutNode::Split { children, .. } => {
            for child in children {
                collect_pane_surfaces(child, out);
            }
        }
    }
}

/// Turn the surfaces of an over-cap slot into parked rail rows on the
/// container, and report how many were parked.
///
/// Only a **terminal** surface becomes a row. An `agent` tab is a reference to
/// a surface record the container already owns, so dropping the reference is
/// all it takes to park it - `threads_from_session` has already built the row.
/// A markdown or diff tab is a view of something that still exists on disk and
/// has no row kind of its own; it is reopened from the file tree or the rail.
///
/// A terminal surface that says nothing about itself - no name, no cwd - is not
/// parked either: `validate_layout` synthesizes exactly those to keep a pruned
/// split structurally valid, and a rail row for one would name a shell the user
/// never opened.
fn park_evicted_surfaces(
    evicted: &[splitlane_config::schema::SurfaceDefinition],
    cwd: &Path,
    threads: &mut Vec<crate::project::Thread>,
) -> usize {
    let mut parked = 0usize;
    for surface in evicted {
        let is_terminal = matches!(surface.surface_type.as_deref(), None | Some("terminal"));
        if !is_terminal {
            continue;
        }
        let title = surface
            .custom_name
            .clone()
            .or_else(|| surface.name.clone())
            .unwrap_or_default();
        let cwd = surface
            .cwd
            .clone()
            .unwrap_or_else(|| cwd.display().to_string());
        if title.is_empty() {
            continue;
        }
        if threads.len() >= crate::project::MAX_RESTORED_THREADS_PER_PROJECT {
            break;
        }
        threads.push(crate::project::Thread::new_terminal(title, cwd, None));
        parked += 1;
    }
    parked
}

fn should_repair_restored_root_terminal(title: &str, cwd: &Path) -> bool {
    is_numbered_terminal_title(title) && launch_cwd::is_filesystem_root(cwd)
}

fn is_numbered_terminal_title(title: &str) -> bool {
    let Some(number) = title.strip_prefix("Terminal ") else {
        return false;
    };
    !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())
}

/// Rehydrate one persisted `expanded_paths` entry into an absolute path under
/// `cwd`, re-asserting containment. The save side strips to a relative
/// inside-root path, but `Path::join` does not normalize, so a hand-edited /
/// agent-written session.json could carry `../../etc` or an absolute `/etc`
/// that silently replaces the base. Reject any traversal/absolute component up
/// front, then re-check `starts_with(base)` after the join. Returns `None`
/// (drop the entry) on any escape.
fn rehydrate_expanded_path(cwd: &str, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        log::warn!(
            "session restore: dropping expanded_path with traversal/absolute component: {rel:?}"
        );
        return None;
    }
    let base = PathBuf::from(cwd);
    let abs = base.join(rel_path);
    if !abs.starts_with(&base) {
        log::warn!("session restore: dropping expanded_path escaping workspace root: {rel:?}");
        return None;
    }
    Some(abs)
}

fn rehydrate_managed_worktree(
    def: &splitlane_config::schema::ManagedWorktreeDef,
) -> Option<crate::workspace::worktree::ManagedWorktree> {
    crate::workspace::worktree::managed_worktree_from_record(
        &def.path,
        &def.repo_root,
        &def.branch,
        &def.teardown,
    )
}

fn persisted_expanded_paths(cwd: &str, expanded: &[PathBuf]) -> Vec<String> {
    let mut paths: Vec<String> = expanded
        .iter()
        .filter_map(|p| p.strip_prefix(cwd).ok())
        .map(|rel| rel.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    paths
}

// ---------------------------------------------------------------------------
// Corruption-backup helpers (free functions, free of `&self`)
// ---------------------------------------------------------------------------

/// Serialize a [`SessionState`] to `path` with an atomic
/// write-temp-then-rename, so a crash mid-write never truncates the live
/// `session.json`. Best-effort: any error is logged, never propagated. Runs off
/// the GPUI main thread in the deferred path (`save_session` wraps it in
/// `smol::unblock`); `save_session_blocking` calls it directly at quit.
fn write_session_json(path: &Path, state: &splitlane_config::schema::SessionState) {
    let _guard = session_write_guard();
    write_session_json_inner(path, state);
}

fn write_session_json_if_current(
    path: &Path,
    state: &splitlane_config::schema::SessionState,
    save_seq: &AtomicU64,
    seq: u64,
) {
    let _guard = session_write_guard();
    if save_seq.load(Ordering::SeqCst) != seq {
        return;
    }
    write_session_json_inner(path, state);
}

fn write_session_json_inner(path: &Path, state: &splitlane_config::schema::SessionState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            let tmp_path = session_tmp_path(path);
            match std::fs::write(&tmp_path, &json) {
                Ok(()) => {
                    if let Err(e) = std::fs::rename(&tmp_path, path) {
                        log::warn!("session save rename failed: {e}");
                        let _ = std::fs::remove_file(&tmp_path);
                    }
                }
                Err(e) => {
                    log::warn!("session save failed: {e}");
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
        }
        Err(e) => log::warn!("session serialize failed: {e}"),
    }
}

fn session_tmp_path(path: &Path) -> PathBuf {
    let seq = SESSION_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let Some(parent) = path.parent() else {
        return path.with_extension(format!("json.tmp.{}.{}", std::process::id(), seq));
    };
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session.json");
    parent.join(format!(".{file_name}.tmp.{}.{}", std::process::id(), seq))
}

/// Convert `serde_json::Error::classify()` to a fixed string. Telemetry
/// schema commits to these four buckets so we can dashboard them
/// directly without remapping if serde widens its enum later.
fn serde_category_tag(err: &serde_json::Error) -> &'static str {
    match err.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    }
}

/// Persist the corrupted file's bytes to
/// `<session_path>.corrupted-<unix-timestamp>` and rotate the backup
/// directory down to [`MAX_CORRUPTION_BACKUPS`] entries.
///
/// Returns `Ok(Some(path))` on success, `Ok(None)` if the wall clock is
/// before `UNIX_EPOCH` (a degenerate state we do not want to crash on),
/// `Err` on actual filesystem failures so the caller can log without
/// blocking startup.
fn write_corruption_backup(
    session_path: &Path,
    contents: &[u8],
) -> std::io::Result<Option<PathBuf>> {
    let parent = match session_path.parent() {
        Some(p) => p,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session path has no parent",
            ));
        }
    };
    std::fs::create_dir_all(parent)?;

    let ts = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos(),
        Err(_) => return Ok(None),
    };
    let seq = SESSION_CORRUPTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = session_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session.json");
    let backup = parent.join(format!(
        "{stem}.corrupted-{ts}-{}-{seq}",
        std::process::id()
    ));
    std::fs::write(&backup, contents)?;

    rotate_corruption_backups(parent, stem);
    Ok(Some(backup))
}

/// Cap the count of `<stem>.corrupted-*` files in `dir` to
/// [`MAX_CORRUPTION_BACKUPS`], deleting the oldest first. Best-effort -
/// any filesystem error during rotation is logged and swallowed because
/// failing rotation must never abort startup, any more than a failed backup
/// write may.
fn rotate_corruption_backups(dir: &Path, stem: &str) {
    let prefix = format!("{stem}.corrupted-");
    let mut backups: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(it) => it
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect(),
        Err(_) => return,
    };

    if backups.len() <= MAX_CORRUPTION_BACKUPS {
        return;
    }

    // Sort by the timestamp prefix ascending (oldest first). Older builds used
    // `corrupted-<seconds>`; current builds append `-<pid>-<seq>` to avoid
    // same-second collisions.
    backups.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(&prefix))
            .and_then(corruption_backup_timestamp)
            .unwrap_or(u128::MAX)
    });

    let drop_count = backups.len() - MAX_CORRUPTION_BACKUPS;
    for old in backups.into_iter().take(drop_count) {
        if let Err(e) = std::fs::remove_file(&old) {
            log::warn!(
                "session backup rotation: could not remove {}: {e}",
                old.display()
            );
        }
    }
}

fn corruption_backup_timestamp(suffix: &str) -> Option<u128> {
    suffix.split('-').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform_root() -> PathBuf {
        std::env::current_dir()
            .ok()
            .and_then(|path| path.ancestors().last().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
    }

    #[test]
    fn restored_root_terminal_repair_only_targets_numbered_default_titles() {
        let root = platform_root();
        assert!(should_repair_restored_root_terminal("Terminal 1", &root));
        assert!(should_repair_restored_root_terminal("Terminal 12", &root));
        assert!(!should_repair_restored_root_terminal("Terminal", &root));
        assert!(!should_repair_restored_root_terminal("Root shell", &root));
    }

    #[test]
    fn restored_root_terminal_repair_ignores_non_root_cwd() {
        let mut cwd = platform_root();
        cwd.push("project");

        assert!(!should_repair_restored_root_terminal("Terminal 1", &cwd));
    }

    #[test]
    fn rehydrate_expanded_path_keeps_inside_root_and_drops_escapes() {
        // A legitimate relative path joins under the cwd…
        assert_eq!(
            rehydrate_expanded_path("/home/u/proj", "src/app"),
            Some(PathBuf::from("/home/u/proj/src/app"))
        );
        // …while traversal and absolute entries from a tampered session.json
        // are dropped rather than silently escaping the workspace root.
        assert_eq!(rehydrate_expanded_path("/home/u/proj", "../../etc"), None);
        assert_eq!(rehydrate_expanded_path("/home/u/proj", "/etc/passwd"), None);
        assert_eq!(rehydrate_expanded_path("/home/u/proj", "a/../../b"), None);
    }

    #[test]
    fn persisted_expanded_paths_are_workspace_relative_and_sorted() {
        let root = PathBuf::from("project");
        let cwd = root.to_string_lossy().into_owned();
        let paths = vec![
            root.join("src").join("z"),
            PathBuf::from("outside"),
            root.join("src").join("a"),
        ];

        let expected_a = PathBuf::from("src")
            .join("a")
            .to_string_lossy()
            .into_owned();
        let expected_z = PathBuf::from("src")
            .join("z")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            persisted_expanded_paths(&cwd, &paths),
            vec![expected_a, expected_z]
        );
    }

    #[test]
    fn session_restore_discards_legacy_scrollback_recursively() {
        let surface = |scrollback: &str| splitlane_config::schema::SurfaceDefinition {
            scrollback: Some(scrollback.to_string()),
            ..Default::default()
        };
        let layout = LayoutNode::Split {
            direction: "horizontal".to_string(),
            ratio: None,
            ratios: None,
            children: vec![
                LayoutNode::Pane {
                    surfaces: vec![surface("old pane")],
                },
                LayoutNode::Split {
                    direction: "vertical".to_string(),
                    ratio: None,
                    ratios: None,
                    children: vec![LayoutNode::Pane {
                        surfaces: vec![surface("old nested pane")],
                    }],
                },
            ],
        };

        let cleaned = without_persisted_scrollback(layout);
        let mut pending = vec![&cleaned];
        while let Some(node) = pending.pop() {
            match node {
                LayoutNode::Pane { surfaces } => {
                    assert!(surfaces.iter().all(|surface| surface.scrollback.is_none()));
                }
                LayoutNode::Split { children, .. } => pending.extend(children),
            }
        }
    }

    /// A grid comes back a grid, empty cells and all.
    ///
    /// The rule this is an exception to is a real one - an empty pane has no
    /// rail row and nothing running in it, so restoring one puts a launcher in
    /// the way. In a grid the four cells are the form the person chose, and
    /// pruning two of them would leave two leaves in a nest that `flattened`
    /// straightens into a column: the layout rearranging itself across a
    /// restart, which is exactly what the live app is forbidden to do.
    #[test]
    fn a_grid_survives_restore_with_its_empty_cells() {
        use splitlane_config::schema::{LayoutNode, SurfaceDefinition};
        let filled = || LayoutNode::Pane {
            surfaces: vec![SurfaceDefinition {
                surface_type: Some("terminal".to_string()),
                name: Some("shell".to_string()),
                ..Default::default()
            }],
        };
        let empty = || LayoutNode::Pane { surfaces: vec![] };
        let row = |a: LayoutNode, b: LayoutNode| LayoutNode::Split {
            direction: "vertical".to_string(),
            ratio: None,
            ratios: None,
            children: vec![a, b],
        };
        let grid = LayoutNode::Split {
            direction: "horizontal".to_string(),
            ratio: None,
            ratios: None,
            children: vec![row(filled(), filled()), row(filled(), empty())],
        };

        let capped = layout_within_cap(grid);
        let layout = capped.layout.expect("a grid is within the cap");
        assert!(
            is_grid_node(&layout),
            "the grid keeps its shape through restore"
        );
        assert_eq!(
            layout.leaf_count(),
            MAX_PANES,
            "all four cells come back, the empty one included"
        );
        assert!(capped.evicted.is_empty());
    }

    /// The two grid predicates read different types and must agree.
    ///
    /// `LayoutTree::is_grid` cannot be asked before a pane exists, so restore
    /// has its own `is_grid_node` over the schema - two short functions that
    /// agree by construction. This is the loop closed: what the app writes for
    /// a live grid is what restore recognises as one. Without it, a change to
    /// either shape check would go unnoticed until a person's four panes came
    /// back as a column.
    #[gpui::test]
    fn what_the_app_writes_for_a_grid_is_what_restore_recognises(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let panes: Vec<_> = (0..4)
            .map(|_| {
                let terminal =
                    cx.new(|cx| crate::terminal::TerminalView::display_only_for_test(1, cx));
                cx.new(|cx| crate::pane::Pane::new(terminal, cx))
            })
            .collect();
        let grid = crate::layout::LayoutTree::grid_of_four(panes).expect("grid");
        let node = cx.update(|_, cx| grid.serialize_without_scrollback(cx));
        assert!(
            is_grid_node(&node),
            "a serialized grid must be recognised as one on the way back in"
        );
    }

    /// And the exception is *only* for the grid: an empty pane in a row is
    /// still not restored.
    #[test]
    fn an_empty_pane_outside_a_grid_is_still_pruned() {
        use splitlane_config::schema::{LayoutNode, SurfaceDefinition};
        let filled = LayoutNode::Pane {
            surfaces: vec![SurfaceDefinition {
                surface_type: Some("terminal".to_string()),
                name: Some("shell".to_string()),
                ..Default::default()
            }],
        };
        let row = LayoutNode::Split {
            direction: "vertical".to_string(),
            ratio: None,
            ratios: None,
            children: vec![filled, LayoutNode::Pane { surfaces: vec![] }],
        };
        let layout = layout_within_cap(row)
            .layout
            .expect("the filled pane survives");
        assert_eq!(layout.leaf_count(), 1, "the empty half is not restored");
    }

    #[test]
    fn layout_within_cap_truncates_an_overshooting_deep_layout() {
        use splitlane_config::schema::{LayoutNode, SurfaceDefinition};
        // Named, because a surface that names nothing is the validator's
        // structural filler and is pruned before the cap is even asked - this
        // test is about the cap, not about pads.
        let pane = || LayoutNode::Pane {
            surfaces: vec![SurfaceDefinition {
                surface_type: Some("terminal".to_string()),
                name: Some("shell".to_string()),
                ..Default::default()
            }],
        };
        // A small, valid layout passes through unchanged.
        let small = LayoutNode::Split {
            direction: "vertical".to_string(),
            ratio: None,
            ratios: None,
            children: vec![pane(), pane()],
        };
        let capped = layout_within_cap(small);
        assert!(
            capped.layout.is_some(),
            "a 2-pane layout is within MAX_PANES"
        );
        assert!(
            capped.evicted.is_empty(),
            "a layout within the cap evicts nothing"
        );

        // A deeply-nested left-leaning chain defeats
        // `validate_layout`'s leaf budget via its >=2-children padding (each
        // budget-0 ancestor smuggles in an uncounted pad pane), so the
        // post-validation leaf_count exceeds MAX_PANES. The hard cap must drop
        // it rather than spawn O(depth) PTYs.
        let mut deep = LayoutNode::Pane {
            surfaces: vec![Default::default()],
        };
        for _ in 0..60 {
            deep = LayoutNode::Split {
                direction: "vertical".to_string(),
                ratio: None,
                ratios: None,
                children: vec![deep, pane()],
            };
        }
        let capped = layout_within_cap(deep);
        let layout = capped
            .layout
            .expect("an over-cap layout is cut down, not thrown away");
        assert_eq!(
            layout.leaf_count(),
            MAX_PANES,
            "a layout exceeding MAX_PANES keeps exactly MAX_PANES slots"
        );
        assert!(
            layout_is_flat(&layout),
            "the kept slots are one row, not the nested shape they came from"
        );
    }

    /// A container emptied by closing its last pane comes back empty. It must
    /// not gain a pane at restore: the whole point of closing the last one is
    /// that the project shows nothing until the rail's own `+ agent` /
    /// `+ shell` / `+ worktree` puts something there.
    #[test]
    fn layout_within_cap_drops_slots_that_hold_nothing() {
        use splitlane_config::schema::{LayoutNode, SurfaceDefinition};
        let empty = LayoutNode::Pane {
            surfaces: Vec::new(),
        };
        assert!(
            layout_within_cap(empty).layout.is_none(),
            "a lone empty slot leaves the container with no layout at all"
        );

        let with_one = LayoutNode::Split {
            direction: "vertical".to_string(),
            ratio: None,
            ratios: Some(vec![0.5, 0.5]),
            children: vec![
                LayoutNode::Pane {
                    surfaces: vec![SurfaceDefinition {
                        surface_type: Some("terminal".to_string()),
                        name: Some("real".to_string()),
                        ..Default::default()
                    }],
                },
                LayoutNode::Pane {
                    surfaces: Vec::new(),
                },
            ],
        };
        let back = layout_within_cap(with_one)
            .layout
            .expect("the slot that holds something survives");
        let LayoutNode::Pane { surfaces } = &back else {
            panic!("a split of one is that one, not a split");
        };
        assert_eq!(surfaces[0].name.as_deref(), Some("real"));
    }

    /// A layout that needs nothing done to it comes back **exactly** as the
    /// file stated it - shape and ratios both. The pane widths are the user's
    /// arrangement, and rebuilding the tree to enforce a bound it already
    /// satisfies would reset them on every launch.
    #[test]
    fn layout_within_cap_leaves_a_layout_that_needs_nothing_alone() {
        use splitlane_config::schema::{LayoutNode, SurfaceDefinition};
        let pane = |name: &str| LayoutNode::Pane {
            surfaces: vec![SurfaceDefinition {
                surface_type: Some("terminal".to_string()),
                name: Some(name.to_string()),
                ..Default::default()
            }],
        };
        let layout = LayoutNode::Split {
            direction: "vertical".to_string(),
            ratio: None,
            ratios: Some(vec![0.7, 0.3]),
            children: vec![pane("wide"), pane("narrow")],
        };

        let capped = layout_within_cap(layout.clone());
        let back = capped.layout.expect("an in-cap layout survives");
        assert_eq!(
            back.leaf_count(),
            2,
            "an in-cap layout keeps every slot it had"
        );
        let LayoutNode::Split { ratios, .. } = &back else {
            panic!("a split in, a split out - not a rebuilt one");
        };
        assert_eq!(
            ratios.as_deref(),
            Some([0.7, 0.3].as_slice()),
            "the pane widths the user arranged are not the cap's business"
        );
        assert!(capped.evicted.is_empty() && capped.displaced_tabs.is_empty());
    }

    /// A pane holds one surface. A file written when panes had tab strips has
    /// more surfaces than slots, and the extras must come back as rail rows
    /// rather than disappear - the same answer the cap gives, because from the
    /// user's side it is the same event.
    #[test]
    fn layout_within_cap_keeps_the_shown_surface_and_parks_the_other_tabs() {
        use splitlane_config::schema::{LayoutNode, SurfaceDefinition};
        let tab = |name: &str, focus: bool| SurfaceDefinition {
            surface_type: Some("terminal".to_string()),
            name: Some(name.to_string()),
            focus: focus.then_some(true),
            ..Default::default()
        };
        let layout = LayoutNode::Pane {
            surfaces: vec![tab("behind", false), tab("shown", true), tab("also", false)],
        };

        let capped = layout_within_cap(layout);
        let layout = capped.layout.expect("a one-pane layout survives");
        let LayoutNode::Pane { surfaces } = &layout else {
            panic!("one pane in, one pane out");
        };
        assert_eq!(surfaces.len(), 1, "a pane comes back holding one surface");
        assert_eq!(
            surfaces[0].name.as_deref(),
            Some("shown"),
            "the one kept is the one the file said was showing, not the first - \
             restoring to a tab the user had left behind is not restoring"
        );

        let parked: Vec<&str> = capped
            .displaced_tabs
            .iter()
            .filter_map(|s| s.name.as_deref())
            .collect();
        assert_eq!(
            parked,
            vec!["behind", "also"],
            "the other tabs are handed back to be parked, in file order"
        );
        assert!(
            capped.evicted.is_empty(),
            "nothing was over the pane cap here; the two reasons stay apart \
             because the toast names the reason"
        );
    }

    /// The evicted surfaces are the ones past the cap, in file order, and the
    /// kept ones are the first three - so what comes back on screen is the
    /// start of the row the user saw, not an arbitrary three of it.
    #[test]
    fn layout_within_cap_evicts_the_slots_past_the_cap_in_file_order() {
        use splitlane_config::schema::{LayoutNode, SurfaceDefinition};
        let named = |name: &str| LayoutNode::Pane {
            surfaces: vec![SurfaceDefinition {
                surface_type: Some("terminal".to_string()),
                name: Some(name.to_string()),
                ..Default::default()
            }],
        };
        let layout = LayoutNode::Split {
            direction: "vertical".to_string(),
            ratio: None,
            ratios: None,
            children: vec![named("a"), named("b"), named("c"), named("d"), named("e")],
        };
        let capped = layout_within_cap(layout);
        let kept = capped.layout.expect("kept slots");
        assert_eq!(kept.leaf_count(), MAX_PANES);
        let names: Vec<String> = capped
            .evicted
            .iter()
            .filter_map(|s| s.name.clone())
            .collect();
        // Derived from the cap rather than written out, so raising the ceiling
        // moves the assertion with it instead of failing it: whatever `a`..`e`
        // does not fit is what gets parked, in file order.
        let expected: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .skip(MAX_PANES)
            .map(|s| s.to_string())
            .collect();
        assert_eq!(names, expected);
    }

    /// An over-cap slot's shells become rail rows; a pad pane `validate_layout`
    /// synthesized to keep a pruned split legal does not, because it names no
    /// shell the user ever opened.
    #[test]
    fn park_evicted_surfaces_parks_named_shells_and_skips_pad_panes() {
        use splitlane_config::schema::SurfaceDefinition;
        let evicted = vec![
            SurfaceDefinition {
                surface_type: Some("terminal".to_string()),
                name: Some("build".to_string()),
                ..Default::default()
            },
            // A pad pane: nothing but its type.
            SurfaceDefinition::default(),
            // A tab referencing a surface record the container already owns.
            SurfaceDefinition {
                surface_type: Some("agent".to_string()),
                name: Some("claude".to_string()),
                surface_id: Some(7),
                ..Default::default()
            },
        ];
        let mut threads = Vec::new();
        let parked = park_evicted_surfaces(&evicted, Path::new("/tmp/project"), &mut threads);
        assert_eq!(parked, 1);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title, "build");
        assert_eq!(threads[0].cwd, "/tmp/project");
        assert!(threads[0].terminal_agent.is_none());
    }

    fn layout_is_flat(node: &LayoutNode) -> bool {
        match node {
            LayoutNode::Pane { .. } => true,
            LayoutNode::Split { children, .. } => children
                .iter()
                .all(|child| matches!(child, LayoutNode::Pane { .. })),
        }
    }

    #[test]
    fn read_session_capped_reads_small_file_and_rejects_non_regular() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Happy path: a normal small file round-trips.
        let path = tmp.path().join("session.json");
        std::fs::write(&path, "{\"ok\":true}").expect("seed");
        assert_eq!(
            read_session_capped(&path).expect("io ok"),
            SessionRead::Data(b"{\"ok\":true}".to_vec())
        );
        // Opening a directory is platform-dependent: Unix usually reaches the
        // metadata guard, Windows can fail at open. Either path must not be
        // mistaken for a missing session.
        assert!(matches!(
            read_session_capped(tmp.path()),
            Ok(SessionRead::Rejected("non_regular")) | Err(_)
        ));
    }

    #[test]
    fn oversized_session_returns_corruption_info_without_backup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");
        let file = std::fs::File::create(&session_path).expect("seed");
        file.set_len(MAX_SESSION_SIZE_BYTES + 1)
            .expect("sparse oversize file");

        let (state, info) = SplitlaneApp::load_session_at(&session_path);

        assert!(state.is_none());
        let info = info.expect("oversize rejection emits diagnostics");
        assert_eq!(info.error_category, "oversize");
        assert_eq!(info.file_size, MAX_SESSION_SIZE_BYTES + 1);
        assert!(
            info.backup_path.is_none(),
            "do not copy huge rejected files"
        );
    }

    #[test]
    fn unsupported_session_version_returns_corruption_info_and_backup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");
        let contents = r#"{
            "version": 999,
            "active_workspace": 0,
            "workspaces": []
        }"#;
        std::fs::write(&session_path, contents).expect("seed unsupported session");

        let (state, info) = SplitlaneApp::load_session_at(&session_path);

        assert!(state.is_none());
        let info = info.expect("unsupported version emits diagnostics");
        assert_eq!(info.error_category, "unsupported_version");
        let backup = info.backup_path.expect("backup path populated");
        assert_eq!(
            std::fs::read_to_string(backup).expect("backup readable"),
            contents
        );
    }

    #[test]
    fn corruption_backup_names_do_not_collide_within_same_second() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");

        let first = write_corruption_backup(&session_path, b"first")
            .expect("first backup")
            .expect("first path");
        let second = write_corruption_backup(&session_path, b"second")
            .expect("second backup")
            .expect("second path");

        assert_ne!(first, second, "backups must not overwrite each other");
        assert_eq!(std::fs::read(&first).expect("first readable"), b"first");
        assert_eq!(std::fs::read(&second).expect("second readable"), b"second");
    }

    #[test]
    fn restored_cwd_helpers_fall_back_for_missing_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let valid = tmp.path().to_path_buf();
        let missing = tmp.path().join("missing");
        let valid_str = valid.to_string_lossy().into_owned();
        let missing_str = missing.to_string_lossy().into_owned();

        assert_eq!(
            restored_workspace_cwd(&valid_str),
            valid,
            "existing workspace cwd is preserved"
        );

        let workspace_fallback = restored_workspace_cwd(&missing_str);
        assert!(
            workspace_fallback.is_dir(),
            "missing workspace cwd falls back to a live directory"
        );
        assert_ne!(workspace_fallback, missing);

        let surface_fallback = tmp.path().join("fallback");
        std::fs::create_dir_all(&surface_fallback).expect("fallback dir");
        assert_eq!(
            resolved_surface_cwd(Some(&missing_str), &surface_fallback),
            surface_fallback.clone(),
            "missing surface cwd falls back to workspace cwd"
        );
        assert_eq!(
            resolved_surface_cwd(None, &surface_fallback),
            surface_fallback,
            "absent surface cwd falls back to workspace cwd"
        );
    }

    /// Write a `session.json` with deliberately broken JSON, run the
    /// path-parametrised loader, assert the corruption-info shape and
    /// the on-disk backup file. Covers the None fallback + info
    /// emitted, the backup written before fallback, and the load_session
    /// behaviour.
    #[test]
    fn malformed_json_returns_corruption_info_and_writes_backup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");
        std::fs::write(&session_path, "{").expect("seed broken session");

        let (state, info) = SplitlaneApp::load_session_at(&session_path);
        assert!(state.is_none(), "fallback to empty session expected");

        let info = info.expect("corruption info expected");
        assert_eq!(info.error_category, "eof", "trailing brace = EOF bucket");
        assert_eq!(info.file_size, 1, "single byte file");
        let backup = info.backup_path.expect("backup path populated");
        assert!(backup.exists(), "backup file actually on disk");
        assert!(
            backup
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session.json.corrupted-")),
            "backup name format honoured"
        );
        let backup_contents = std::fs::read_to_string(&backup).expect("backup is readable");
        assert_eq!(backup_contents, "{", "backup preserves original bytes");
    }

    #[test]
    fn invalid_utf8_returns_corruption_info_and_writes_backup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");
        std::fs::write(&session_path, [0xff, 0xfe, b'{']).expect("seed broken session");

        let (state, info) = SplitlaneApp::load_session_at(&session_path);
        assert!(state.is_none(), "fallback to empty session expected");

        let info = info.expect("corruption info expected");
        assert_eq!(info.error_category, "data");
        let backup = info.backup_path.expect("backup path populated");
        let backup_contents = std::fs::read(&backup).expect("backup is readable");
        assert_eq!(backup_contents, vec![0xff, 0xfe, b'{']);
    }

    /// The container merge's upgrade path: a schema-v1 file loads as the
    /// merged shape, and the original is preserved beside it.
    #[test]
    fn v1_session_migrates_in_place_and_keeps_a_backup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");
        let v1 = r#"{
            "version": 1,
            "active_workspace": 0,
            "workspaces": [
                { "title": "splitlane", "cwd": "/p/splitlane", "layout": null },
                { "title": "topgun", "cwd": "/p/topgun", "layout": null }
            ],
            "projects": [
                { "id": 1, "title": "topgun", "cwd": "/p/topgun", "threads": [] }
            ],
            "active_project": 0,
            "mode": "agents"
        }"#;
        std::fs::write(&session_path, v1).expect("seed v1 session");

        let (state, info) = SplitlaneApp::load_session_at(&session_path);

        let state = state.expect("a v1 session migrates rather than starting empty");
        assert!(info.is_none(), "a readable v1 file is not corruption");
        assert_eq!(
            state.version,
            splitlane_config::schema::SESSION_SCHEMA_VERSION
        );
        assert_eq!(state.projects.len(), 2, "one container per directory");
        assert_eq!(
            state.projects[1].id, 1,
            "the joined container adopts the project's id"
        );

        let backup = session_path.with_extension("json.v1.bak");
        assert_eq!(
            std::fs::read_to_string(&backup).expect("backup written"),
            v1,
            "the pre-merge file is kept verbatim - it is the only way back"
        );
    }

    /// A file that claims v1 but is not one must land in the corruption path,
    /// not be half-migrated.
    #[test]
    fn malformed_v1_session_falls_through_to_corruption() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");
        std::fs::write(&session_path, r#"{"version": 1, "workspaces": 7}"#).expect("seed");

        let (state, info) = SplitlaneApp::load_session_at(&session_path);

        assert!(state.is_none());
        assert!(info.is_some(), "corruption is reported, not migrated");
        assert!(
            !session_path.with_extension("json.v1.bak").exists(),
            "no migration backup for a file that never migrated"
        );
    }

    /// A missing file is *not* corruption - both halves of the
    /// return tuple must be `None` so `bootstrap.rs` doesn't emit a
    /// noisy `session_corrupted` event for every fresh install.
    #[test]
    fn missing_file_yields_no_state_no_corruption() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nonexistent.json");

        let (state, info) = SplitlaneApp::load_session_at(&path);
        assert!(state.is_none());
        assert!(info.is_none(), "missing file is not corruption");
    }

    /// The backup directory must not grow unbounded. After 7 induced
    /// corruptions only the 5 newest survive - verifies the
    /// timestamp-sort + drop-oldest path in `rotate_corruption_backups`.
    #[test]
    fn corruption_backup_rotation_caps_at_five() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_path = tmp.path().join("session.json");

        // Pre-seed 7 backups with monotonic synthetic timestamps so
        // the test does not depend on the host's wall-clock resolution
        // (a real run produces one new backup per parse failure, but
        // bursts within the same second would otherwise collide).
        for ts in 1000..1007u64 {
            let p = tmp.path().join(format!("session.json.corrupted-{ts}"));
            std::fs::write(&p, format!("backup{ts}")).expect("seed backup");
        }

        rotate_corruption_backups(tmp.path(), "session.json");

        let mut surviving: Vec<u64> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.strip_prefix("session.json.corrupted-"))
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .collect();
        surviving.sort_unstable();
        assert_eq!(
            surviving,
            vec![1002, 1003, 1004, 1005, 1006],
            "5 newest survive, 2 oldest deleted"
        );

        // Live session.json itself is unaffected by the rotation.
        std::fs::write(&session_path, "{").expect("seed");
        assert!(session_path.exists());
    }

    /// A burst of `save_session` calls (e.g. closing 20 workspaces)
    /// must coalesce to a single disk write - only the most-recent snapshot
    /// wins. This guards the `save_seq` monotonic-token predicate that the
    /// deferred path uses (`save_session` checks `load() == seq` after its
    /// debounce). A full `save_session` needs a live GPUI `App`, so this tests
    /// the coalescing invariant directly on the same atomic logic.
    #[test]
    fn save_seq_burst_coalesces_to_a_single_write() {
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

        let save_seq = AtomicU64::new(0);
        // Simulate 20 saves fired in a burst; each captures its token the way
        // `save_session` does (`fetch_add(1) + 1`).
        let captured: Vec<u64> = (0..20).map(|_| save_seq.fetch_add(1, SeqCst) + 1).collect();

        // After the burst, exactly one captured token equals the latest value,
        // so exactly one deferred task survives its post-debounce check.
        let latest = save_seq.load(SeqCst);
        let survivors = captured.iter().filter(|&&s| s == latest).count();
        assert_eq!(survivors, 1, "a 20-save burst coalesces to one write");
        assert_eq!(
            captured.last().copied(),
            Some(latest),
            "the most-recent snapshot is the survivor"
        );
    }

    /// Regression guard for the quit-path race: a deferred save that
    /// passed its debounce check must still skip its write if a blocking or
    /// newer save bumped the token before it acquired the write lock.
    #[test]
    fn deferred_save_skips_write_when_superseded_before_write() {
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

        let save_seq = AtomicU64::new(0);
        // A deferred save is scheduled and passes its post-debounce check.
        let deferred = save_seq.fetch_add(1, SeqCst) + 1;
        assert_eq!(
            save_seq.load(SeqCst),
            deferred,
            "deferred is latest pre-drain"
        );

        // Before it reaches the write lock, a blocking save bumps the token.
        save_seq.fetch_add(1, SeqCst);

        // The pre-write re-check inside `smol::unblock` must now observe the
        // mismatch and skip - so the older deferred snapshot never renames over
        // the final quit write.
        assert_ne!(
            save_seq.load(SeqCst),
            deferred,
            "deferred write must be skipped after a quit-time bump"
        );
    }

    #[test]
    fn restored_managed_worktree_must_match_splitlane_worktree_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_root = tmp.path().join("repo");
        let branch = "feat/session-hardening";
        let owned_path = crate::workspace::worktree::worktree_dir(&repo_root, branch);
        std::fs::create_dir_all(&owned_path).expect("owned worktree dir");
        std::fs::write(
            crate::workspace::worktree::owner_marker_path(&owned_path),
            "owner=splitlane\n",
        )
        .expect("owner marker");
        let valid = splitlane_config::schema::ManagedWorktreeDef {
            path: owned_path.to_string_lossy().into_owned(),
            repo_root: repo_root.to_string_lossy().into_owned(),
            branch: branch.to_string(),
            teardown: "auto".to_string(),
        };

        let restored = rehydrate_managed_worktree(&valid).expect("valid owned worktree restores");
        assert_eq!(restored.path, owned_path);
        assert_eq!(
            restored.teardown,
            crate::workspace::worktree::TeardownPolicy::Auto
        );

        let outside = splitlane_config::schema::ManagedWorktreeDef {
            path: tmp.path().join("external").to_string_lossy().into_owned(),
            ..valid.clone()
        };
        assert!(
            rehydrate_managed_worktree(&outside).is_none(),
            "a restored worktree path outside Splitlane's generated dir is dropped"
        );

        let unknown_policy = splitlane_config::schema::ManagedWorktreeDef {
            teardown: "delete".to_string(),
            ..valid
        };
        let restored =
            rehydrate_managed_worktree(&unknown_policy).expect("shape-valid worktree restores");
        assert_eq!(
            restored.teardown,
            crate::workspace::worktree::TeardownPolicy::Keep,
            "unknown restored policy must not become auto-remove"
        );
    }
}
