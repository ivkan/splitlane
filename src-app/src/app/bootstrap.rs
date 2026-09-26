//! `SplitlaneApp::new()` - the application constructor.
//!
//! Wires the title bar, IPC server, config watcher, git-dir watcher, update
//! checker, and all background tickers (50 ms IPC poll, 30 s git fallback,
//! 30 s stale-PID sweep). Restores a saved session or creates a fresh
//! single-workspace state.
//!
//! Extracted from `main.rs` - pure code-motion, behaviour unchanged.

use gpui::{AppContext, Context};
use notify::Watcher;

use crate::launch_cwd;
use crate::pane::Pane;
use crate::telemetry;
use crate::terminal::TerminalView;
use crate::terminal::blink::{BlinkPhase, BlinkPhaseGlobal, CURSOR_BLINK_INTERVAL};
use crate::window_chrome::title_bar;
use crate::workspace::{Workspace, next_workspace_id};
use crate::{SplitlaneApp, ipc, keybindings, update};

impl SplitlaneApp {
    /// What a launch with nothing to restore opens: a container for the
    /// directory a terminal launch was started in, or no container at all -
    /// the rail and the content area both draw that state. See
    /// `launch_cwd::first_container_cwd` for why the home folder is not it.
    fn first_containers(cx: &mut Context<Self>) -> (Vec<Workspace>, usize) {
        let workspaces = launch_cwd::first_container_cwd()
            .map(|cwd| Self::default_workspace(cwd, cx))
            .into_iter()
            .collect();
        (workspaces, 0)
    }

    fn default_workspace(cwd: std::path::PathBuf, cx: &mut Context<Self>) -> Workspace {
        let ws_id = next_workspace_id();
        let terminal_cwd = cwd.clone();
        let terminal = cx.new(|cx| TerminalView::with_cwd(ws_id, Some(terminal_cwd), None, cx));
        cx.subscribe(&terminal, Self::handle_terminal_event)
            .detach();
        let pane = cx.new(|cx| Pane::new(terminal, cx));
        cx.subscribe(&pane, Self::handle_pane_event).detach();
        let dir_name = launch_cwd::title_for_cwd_or(&cwd, "Terminal 1");
        let ws = Workspace::with_cwd_and_id(ws_id, dir_name, cwd, pane);
        Self::spawn_initial_git_stats(ws_id, ws.cwd.clone(), cx);
        ws
    }

    /// A container for the user's home directory, built without a saved
    /// record. Used when a session carries free chats but no container for
    /// `~` to fold them into.
    fn home_container(home: &str, cx: &mut Context<Self>) -> Workspace {
        let ws_id = next_workspace_id();
        let cwd = std::path::PathBuf::from(home);
        let terminal = cx.new(|cx| TerminalView::with_cwd(ws_id, Some(cwd.clone()), None, cx));
        cx.subscribe(&terminal, Self::handle_terminal_event)
            .detach();
        let pane = cx.new(|cx| Pane::new(terminal, cx));
        cx.subscribe(&pane, Self::handle_pane_event).detach();
        let title = launch_cwd::title_for_cwd_or(&cwd, "Home");
        let ws = Workspace::with_cwd_and_id(ws_id, title, cwd, pane);
        Self::spawn_initial_git_stats(ws_id, ws.cwd.clone(), cx);
        ws
    }

    pub(crate) fn spawn_telemetry_flusher(
        telemetry: std::sync::Arc<telemetry::client::TelemetryClient>,
        cx: &mut Context<Self>,
    ) {
        cx.background_spawn(async move {
            loop {
                smol::Timer::after(std::time::Duration::from_secs(5)).await;
                let client = std::sync::Arc::clone(&telemetry);
                if !client.is_active() {
                    break;
                }
                smol::unblock(move || client.poll_flush()).await;
            }
        })
        .detach();
    }

    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let title_bar = cx.new(title_bar::TitleBar::new);
        cx.subscribe(&title_bar, Self::handle_title_bar_event)
            .detach();
        let (ipc_rx, ipc_status, event_bus) = ipc::start_server();

        // Install the shared cursor-blink phase as a GPUI global
        // before any `TerminalView` is constructed. Each `TerminalView`
        // reads the global in `with_cwd` and observes the entity, so all
        // visible cursors blink in phase. One bootstrap-spawned loop
        // toggles `phase.visible` every 530 ms - replaces N per-terminal
        // `smol::Timer` loops with a single ticker for the whole app.
        let blink_phase = cx.new(|_| BlinkPhase::default());
        cx.set_global(BlinkPhaseGlobal(blink_phase.clone()));
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                loop {
                    smol::Timer::after(CURSOR_BLINK_INTERVAL).await;
                    // Read the entity fresh from the global on every tick
                    // to keep this loop consistent with the existing
                    // git-watcher / IPC-poll patterns in this file: all of
                    // them go through `this.update(cx, |app, cx| ...)` and
                    // pull whatever they need from `cx`/`app` inside the
                    // closure rather than capturing it. Capturing the
                    // entity once would also be safe (the App owns the
                    // strong ref via the global; clones at app teardown
                    // are dropped together) - consistency wins.
                    let result = cx.update(|cx| {
                        this.update(cx, |_app: &mut Self, cx: &mut Context<Self>| {
                            let phase = cx.global::<BlinkPhaseGlobal>().0.clone();
                            phase.update(cx, |p, cx| {
                                p.visible = !p.visible;
                                cx.notify();
                            });
                        })
                    });
                    if result.is_err() {
                        break;
                    }
                }
            },
        )
        .detach();

        // ConfigWatcher: background thread detects file changes (300ms debounce),
        // stores parsed config in a shared slot for the 50ms poll loop to pick up.
        // Note: `start()` moves the OS watcher into a background thread, so the
        // `ConfigWatcher` struct itself can be safely dropped after starting.
        let pending_config = std::sync::Arc::new(std::sync::Mutex::new(
            None::<splitlane_config::schema::SplitlaneConfig>,
        ));
        let pending_config_writer = std::sync::Arc::clone(&pending_config);
        if let Some(Err(e)) = splitlane_config::watcher::ConfigWatcher::new(std::sync::Arc::new(
            move |cfg: splitlane_config::schema::SplitlaneConfig| {
                *pending_config_writer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(cfg);
            },
        ))
        .map(|config_watcher| config_watcher.start())
        {
            log::warn!("config watcher failed to start: {e}; config hot-reload disabled");
        }

        // Dedicated theme watcher. Mirrors `ConfigWatcher` shape but
        // signals via an `Arc<AtomicBool>` rather than carrying a payload -
        // theme invalidation is a tristate "did the file change" question,
        // and the actual `TerminalTheme` is recomputed lazily by
        // `active_theme()` on the next render. The 50 ms poll loop drains
        // this flag and calls `cx.notify()` to schedule the repaint. On
        // init failure the historical 500 ms polling fallback inside
        // `active_theme()` keeps the UI responsive.
        let theme_changed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let theme_changed_writer = std::sync::Arc::clone(&theme_changed);
        match crate::theme::ThemeWatcher::new(std::sync::Arc::new(move || {
            theme_changed_writer.store(true, std::sync::atomic::Ordering::Release);
        })) {
            Some(watcher) => {
                if let Err(e) = watcher.start() {
                    log::warn!(
                        "theme watcher failed to start: {e}; falling back to 500 ms polling"
                    );
                }
            }
            None => {
                log::warn!("theme watcher: no config dir resolved; falling back to 500 ms polling");
            }
        }

        // Background update check is deferred until after the telemetry
        // client is constructed below - `spawn_check` now takes the
        // client by Arc so it can emit `update_check_started` /
        // `update_available`, and the client doesn't exist
        // this early in bootstrap.

        // Restore session or create a single default workspace. The
        // tuple's second component carries forensic context when
        // `session.json` was unparseable; we hold onto it and
        // emit the `session_corrupted` PostHog event after the
        // telemetry client is constructed below - load_session itself
        // runs too early in bootstrap to call `self.telemetry`.
        let (saved_session, session_corruption) = Self::load_session();

        // Pull the Agents-view bits out of
        // the saved session BEFORE the workspaces match consumes it.
        // Restore the diff scope (an
        // unknown / absent value falls back to the default, Project).
        let restored_diff_scope = saved_session
            .as_ref()
            .and_then(|s| s.diff_scope.as_deref())
            .and_then(crate::diff::DiffScope::from_persisted)
            .unwrap_or_default();
        // The rail's width, clamped rather than trusted: the file is the
        // user's and can carry anything.
        let restored_rail_width = saved_session
            .as_ref()
            .and_then(|s| s.rail_width)
            .map(crate::app::rail_resize::clamp_rail_width)
            .unwrap_or(crate::app::agents_view_actions::RAIL_WIDTH);
        // Same contract for the Files tree's own width.
        // The reading the last run ended on becomes the one the first forecast
        // of this run subtracts from. It arrives as `previous` rather than as
        // the current state, because the current state has to come from a read
        // this run actually made - a number on screen must be one we just
        // fetched, and only the thing we compare it against may be older.
        let restored_limits = saved_session
            .as_ref()
            .and_then(|s| s.limits_reading)
            .map(
                |reading| crate::app::agents_sidebar::limits_footer::ClaudeLimits {
                    previous: Some(crate::app::agents_sidebar::limits_footer::PreviousReading {
                        at: reading.at,
                        // The file names Claude's two windows by key, because
                        // that is what the endpoint sends; the reading is keyed
                        // by window *length* the moment it is in memory, which
                        // is the identity a second vendor also states. The map
                        // between the two lives here, at the schema's edge, and
                        // nowhere else.
                        windows: [
                            reading
                                .five_hour
                                .map(|u| (Some(crate::vendor_limits::CLAUDE_SESSION_MINUTES), u)),
                            reading
                                .seven_day
                                .map(|u| (Some(crate::vendor_limits::CLAUDE_WEEKLY_MINUTES), u)),
                        ]
                        .into_iter()
                        .flatten()
                        .collect(),
                    }),
                    ..Default::default()
                },
            )
            .unwrap_or_default();
        let restored_files_width = saved_session
            .as_ref()
            .and_then(|s| s.files_width)
            .map(crate::app::files_sidebar::clamp_files_width)
            .unwrap_or(crate::app::files_sidebar::FILES_SIDEBAR_WIDTH);
        // One restore budget shared by every container's agent surfaces: a
        // session.json cannot fan out into thousands of PTYs however it
        // distributes them across containers.
        let mut remaining_threads = crate::project::MAX_RESTORED_TOTAL_THREADS;
        // Bump the process-wide ID counter past EVERY id the session file
        // carries - containers and surfaces alike, including any this build
        // drops at a restore cap - before anything is minted. A restored
        // container keeps its persisted id, so a fresh id landing on one
        // would be a collision, not a coincidence. Idempotent.
        if let Some(session) = saved_session.as_ref() {
            crate::workspace::bump_next_id_to(crate::project::max_persisted_id(session) + 1);
        }

        // Live PTY views of the agent surfaces restored inside a slot; seeded
        // into the warm terminal cache once the app struct exists below.
        let mut restored_agent_views: Vec<(
            u64,
            gpui::Entity<crate::terminal::view::TerminalView>,
        )> = Vec::new();
        // `(surface id, that PTY's output count at launch)` for every surface
        // restore marked `starting`. See where it is filled below.
        let mut starting_baselines: Vec<(u64, u64)> = Vec::new();
        // Shells a restored layout had in a fourth (or later) slot. They come
        // back parked on their container; the toast below is what keeps that
        // from being a silent rearrangement of the user's screen.
        let mut parked_over_cap = 0usize;
        let mut parked_extra_tabs = 0usize;
        let (mut workspaces, active_idx) = match saved_session.as_ref() {
            Some(session) => {
                log::info!(
                    "restoring session: {} container(s), {} chat(s)",
                    session.projects.len(),
                    session.chats.len(),
                );
                let restored = Self::restore_workspaces(session, &mut remaining_threads, cx);
                // The output baseline for every surface restore marked
                // `starting`, which is what lets the word end at that pane's
                // first output rather than surviving a pass for want of
                // anything to compare against. `restore_workspaces` is an
                // associated function and has no app to write it to; here is
                // the first moment both halves exist.
                starting_baselines = restored
                    .workspaces
                    .iter()
                    .flat_map(|ws| ws.threads.iter())
                    .filter(|thread| thread.status == crate::project::ThreadStatus::Starting)
                    .filter_map(|thread| {
                        let view = restored
                            .agent_views
                            .iter()
                            .find(|(id, _)| *id == thread.id)
                            .map(|(_, view)| view)?;
                        Some((thread.id, view.read(cx).terminal.output_generation))
                    })
                    .collect();
                restored_agent_views = restored.agent_views;
                parked_over_cap = restored.parked_over_cap;
                parked_extra_tabs = restored.parked_extra_tabs;
                if restored.workspaces.is_empty() {
                    // Closing every project and quitting is a state a person
                    // chose, so it is restored as one rather than papered over.
                    log::info!("session restore: session contained no restorable containers");
                    Self::first_containers(cx)
                } else {
                    (restored.workspaces, restored.active_idx)
                }
            }
            None => Self::first_containers(cx),
        };

        // Groups come back only with a project in them: a project capped out
        // of the restore does not keep its group alive, and a hand-edited
        // reference to a group the file does not carry is no group.
        let mut restored_groups = saved_session
            .as_ref()
            .map(|s| crate::app::project_groups::groups_from_session(&s.groups))
            .unwrap_or_default();
        crate::app::project_groups::reconcile_groups(
            &mut restored_groups,
            workspaces.iter_mut().map(|ws| &mut ws.group),
        );

        // Chats draw from the same budget, and deliberately AFTER the
        // containers: they used to get the remainder, and restoring them first
        // would silently trade a container's agent surfaces for free chats.
        let restored_chats: Vec<crate::project::Thread> = saved_session
            .as_ref()
            .map(|s| {
                if s.chats.len() > crate::project::MAX_RESTORED_CHATS {
                    log::warn!(
                        "session restore: {} chat(s) exceeds cap {}, restoring the first {}",
                        s.chats.len(),
                        crate::project::MAX_RESTORED_CHATS,
                        crate::project::MAX_RESTORED_CHATS
                    );
                }
                s.chats
                    .iter()
                    .take(crate::project::MAX_RESTORED_CHATS)
                    .filter_map(|chat| {
                        crate::project::thread_from_surface_with_budget(
                            chat,
                            &mut remaining_threads,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Fold the chats into the container for the home directory, creating
        // it when the session has none. This is the merge the design asked
        // for: one container type, and a surface anchored on `~` belongs to
        // the container for `~` like any other.
        let mut workspaces = workspaces;
        if !restored_chats.is_empty() {
            let home = crate::app::project_ops::home_container_cwd();
            let home_idx = match workspaces
                .iter()
                .position(|ws| splitlane_config::schema::same_directory(&ws.cwd, &home))
            {
                Some(idx) => Some(idx),
                None if workspaces.len() < crate::workspace::MAX_WORKSPACES => {
                    workspaces.push(Self::home_container(&home, cx));
                    Some(workspaces.len() - 1)
                }
                None => None,
            };
            match home_idx {
                Some(idx) => {
                    log::info!(
                        "session restore: folding {} free chat(s) into the container for {home}",
                        restored_chats.len()
                    );
                    workspaces[idx].threads.extend(restored_chats);
                }
                None => log::warn!(
                    "session restore: {} free chat(s) dropped - the container list is full",
                    restored_chats.len()
                ),
            }
        }
        let workspaces = workspaces;

        // What the status bar's restore indicator states. Only a real restore
        // counts: a first run that created a default container restored
        // nothing, and the indicator says nothing rather than "restored 1".
        let restore_summary = saved_session.as_ref().map(|_| {
            let surfaces: usize = workspaces
                .iter()
                .map(|ws| ws.threads.len() + ws.root.as_ref().map_or(0, |root| root.leaf_count()))
                .sum();
            crate::app::status_bar::RestoreSummary {
                at_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                surfaces,
            }
        });

        // A session written before the diff became a pane's content can name
        // it as the selection without recording that it is open. Reconcile
        // towards the selection: what it was showing must have a row.
        let restored_diff_container = saved_session
            .as_ref()
            .and_then(|s| s.agents_target.as_ref())
            .and_then(|target| crate::project::agents_target_from_session(target, &workspaces))
            .and_then(|target| match target {
                crate::project::AgentsTarget::Diff { ws_idx } => Some(ws_idx),
                _ => None,
            });
        let mut workspaces = workspaces;
        if let Some(ws_idx) = restored_diff_container
            && let Some(container) = workspaces.get_mut(ws_idx)
            && container.diff_surface.is_none()
        {
            container.diff_surface = Some(crate::workspace::next_id());
        }
        let workspaces = workspaces;
        // `active_workspace` is the only source for which container is active
        // now; the selection that used to override it is gone.
        let active_idx = active_idx.min(workspaces.len().saturating_sub(1));

        // Setup notify file watcher for .git directories
        let (git_event_tx, git_event_rx) = std::sync::mpsc::channel();
        let mut git_watcher = match notify::recommended_watcher(git_event_tx) {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("git file watcher unavailable: {e}. Falling back to polling.");
                None
            }
        };
        let mut git_watch_counts = std::collections::HashMap::new();
        // Watch all workspaces' .git directories
        if let Some(ref mut watcher) = git_watcher {
            for ws in &workspaces {
                if let Some(ref git_dir) = ws.git_dir {
                    if let Err(e) = watcher.watch(git_dir, notify::RecursiveMode::NonRecursive) {
                        log::warn!("git watcher: failed to watch {}: {e}", git_dir.display());
                    } else {
                        *git_watch_counts.entry(git_dir.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        // Poll git watcher events with 300ms debounce.
        // Filter: only HEAD and index matter. NonRecursive mode limits events to
        // top-level entries of .git/ so no subdirectory false positives.
        // On debounce fire, run git probes off main thread and apply results.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let debounce = std::time::Duration::from_millis(300);
                let mut last_event = std::time::Instant::now() - debounce;
                let mut pending = false;
                let mut pending_git_dirs = std::collections::HashSet::<std::path::PathBuf>::new();

                loop {
                    smol::Timer::after(std::time::Duration::from_millis(200)).await;

                    // Drain events from the watcher channel, collect affected .git dirs
                    let new_dirs = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, _cx: &mut Context<Self>| {
                            let mut dirs = Vec::new();
                            while let Ok(event) = app.git_event_rx.try_recv() {
                                if let Ok(ref ev) = event {
                                    for p in &ev.paths {
                                        if matches!(
                                            p.file_name().and_then(|n| n.to_str()),
                                            Some("HEAD" | "index")
                                        ) && let Some(parent) = p.parent()
                                        {
                                            dirs.push(parent.to_path_buf());
                                        }
                                    }
                                }
                            }
                            dirs
                        })
                    });

                    match new_dirs {
                        Ok(dirs) if !dirs.is_empty() => {
                            pending_git_dirs.extend(dirs);
                            last_event = std::time::Instant::now();
                            pending = true;
                        }
                        Ok(_) => {}
                        Err(_) => break, // app shutting down
                    }

                    // Debounce: fire after 300ms of quiet
                    if pending && last_event.elapsed() >= debounce {
                        pending = false;
                        let affected_dirs = std::mem::take(&mut pending_git_dirs);
                        log::debug!(
                            "git watcher: debounced event fired for {} dir(s)",
                            affected_dirs.len()
                        );

                        // Collect CWDs of affected workspaces (main thread)
                        let cwds = cx.update(|cx| {
                            this.update(cx, |app: &mut Self, _cx: &mut Context<Self>| {
                                app.workspaces
                                    .iter()
                                    .filter(|ws| {
                                        ws.git_dir
                                            .as_ref()
                                            .is_some_and(|gd| affected_dirs.contains(gd))
                                    })
                                    .map(|ws| ws.cwd.clone())
                                    .collect::<Vec<String>>()
                            })
                        });

                        let cwds = match cwds {
                            Ok(c) => c,
                            Err(_) => break,
                        };

                        if cwds.is_empty() {
                            continue;
                        }

                        // Run git probes off main thread
                        let results = smol::unblock(move || {
                            cwds.into_iter()
                                .map(|cwd| {
                                    let (branch, is_repo) = crate::workspace::detect_branch(&cwd);
                                    let stats = crate::workspace::GitDiffStats::from_cwd(&cwd);
                                    (cwd, branch, is_repo, stats)
                                })
                                .collect::<Vec<_>>()
                        })
                        .await;

                        // Apply results to matching workspaces (main thread)
                        let apply = cx.update(|cx| {
                            this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                                let mut changed = false;
                                for (cwd, branch, is_repo, stats) in &results {
                                    if app.apply_git_state_for_cwd(
                                        cwd,
                                        branch.clone(),
                                        *is_repo,
                                        stats.clone(),
                                    ) {
                                        changed = true;
                                    }
                                }
                                if changed {
                                    cx.notify();
                                }
                                // `HEAD` or `index` moved, which changes every
                                // tree mark without touching one worktree file
                                // - so the tree's own watcher will never see
                                // it. Same debounced batch, no timer of its own.
                                app.spawn_files_git_marks(cx);
                            })
                        });
                        if apply.is_err() {
                            break;
                        }
                    }
                }
            },
        )
        .detach();

        // Files-sidebar watcher drain loop. Mirrors the git
        // loop above: poll the per-open watch channel, coalesce affected parent
        // dirs, debounce ~100ms with a 500ms hard-flush ceiling (so a
        // continuous stream like `git checkout` still flushes), then re-read
        // only the affected cached directories. A notify overflow/`Rescan`
        // signal forces a root re-read.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let debounce = std::time::Duration::from_millis(100);
                let ceiling = std::time::Duration::from_millis(500);
                const FILES_EVENT_DRAIN_MAX_PER_TICK: usize = 512;
                let mut first_pending: Option<std::time::Instant> = None;
                let mut last_event = std::time::Instant::now();
                let mut pending_dirs = std::collections::HashSet::<std::path::PathBuf>::new();
                let mut need_rescan = false;

                loop {
                    smol::Timer::after(std::time::Duration::from_millis(50)).await;

                    // Drain the watch channel → affected parent dirs + rescan flag.
                    let drained = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, _cx: &mut Context<Self>| {
                            let mut dirs = Vec::new();
                            let mut rescan = false;
                            let mut watcher_failed = false;
                            let mut drained_events = 0usize;
                            if let Some(rx) = &app.files_event_rx {
                                for _ in 0..FILES_EVENT_DRAIN_MAX_PER_TICK {
                                    match rx.try_recv() {
                                        Ok(Ok(ev)) => {
                                            drained_events += 1;
                                            if ev.need_rescan() {
                                                rescan = true;
                                            }
                                            for p in &ev.paths {
                                                if let Some(parent) = p.parent() {
                                                    dirs.push(parent.to_path_buf());
                                                }
                                            }
                                        }
                                        Ok(Err(err)) => {
                                            drained_events += 1;
                                            log::warn!(
                                                "files watcher error: {err}; falling back to on-expand reads"
                                            );
                                            rescan = true;
                                            watcher_failed = true;
                                            break;
                                        }
                                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                            log::warn!(
                                                "files watcher disconnected; falling back to on-expand reads"
                                            );
                                            watcher_failed = true;
                                            break;
                                        }
                                    }
                                }
                                if drained_events == FILES_EVENT_DRAIN_MAX_PER_TICK {
                                    tracing::debug!(
                                        target: "splitlane_app::files_sidebar",
                                        "files watcher drain capped at {FILES_EVENT_DRAIN_MAX_PER_TICK} events for this tick"
                                    );
                                }
                            }
                            if watcher_failed {
                                app.files_watcher = None;
                                app.files_event_rx = None;
                            }
                            (dirs, rescan)
                        })
                    });

                    let (dirs, rescan) = match drained {
                        Ok(d) => d,
                        Err(_) => break, // app shutting down
                    };

                    if !dirs.is_empty() || rescan {
                        if first_pending.is_none() {
                            first_pending = Some(std::time::Instant::now());
                        }
                        last_event = std::time::Instant::now();
                        pending_dirs.extend(dirs);
                        need_rescan |= rescan;
                    }

                    // Fire after a quiet debounce window OR once the hard
                    // ceiling elapses under a continuous event stream.
                    let should_fire = first_pending.is_some_and(|start| {
                        last_event.elapsed() >= debounce || start.elapsed() >= ceiling
                    });
                    if should_fire {
                        first_pending = None;
                        let affected: Vec<std::path::PathBuf> =
                            std::mem::take(&mut pending_dirs).into_iter().collect();
                        let rescan = std::mem::replace(&mut need_rescan, false);
                        let applied = cx.update(|cx| {
                            this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                                app.refresh_files_dirs(affected, rescan, cx);
                            })
                        });
                        if applied.is_err() {
                            break;
                        }
                    }
                }
            },
        )
        .detach();

        // Poll automation channels every 50 ms.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                loop {
                    smol::Timer::after(std::time::Duration::from_millis(50)).await;
                    let result = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                            app.process_automation_tick(cx);
                        })
                    });
                    if result.is_err() {
                        break;
                    }
                }
            },
        )
        .detach();

        // Config hot-reload is now driven by ConfigWatcher (notify crate, 300ms debounce).
        // Changes are picked up in the 50ms IPC poll loop below via process_config_changes().

        // Fallback: poll git metadata for all workspaces every 30s.
        // Primary detection is event-driven (the notify watcher above).
        // This timer catches edge cases where file system events are missed.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                loop {
                    smol::Timer::after(std::time::Duration::from_secs(30)).await;

                    // Phase 1: collect container CWDs (cheap, main thread).
                    // Dedup so two containers on one directory only fire one
                    // subprocess per tick.
                    let cwds = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, _cx: &mut Context<Self>| {
                            let mut seen = std::collections::HashSet::new();
                            app.workspaces
                                .iter()
                                .filter(|ws| seen.insert(ws.cwd.clone()))
                                .map(|ws| ws.cwd.clone())
                                .collect::<Vec<_>>()
                        })
                    });
                    let cwds = match cwds {
                        Ok(c) => c,
                        Err(_) => break,
                    };

                    // Phase 2: run git probes off main thread
                    let results = smol::unblock(move || {
                        cwds.into_iter()
                            .map(|cwd| {
                                let (branch, is_repo) = crate::workspace::detect_branch(&cwd);
                                let stats = crate::workspace::GitDiffStats::from_cwd(&cwd);
                                (cwd, branch, is_repo, stats)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;

                    // Phase 3: apply results (cheap, main thread)
                    let apply = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                            let mut changed = false;
                            for (cwd, branch, is_repo, stats) in &results {
                                if app.apply_git_state_for_cwd(
                                    cwd,
                                    branch.clone(),
                                    *is_repo,
                                    stats.clone(),
                                ) {
                                    changed = true;
                                }
                            }
                            if changed {
                                cx.notify();
                            }
                        })
                    });
                    if apply.is_err() {
                        break;
                    }
                }
            },
        )
        .detach();

        // The Claude plan's limits: one read shortly after launch, then one
        // every `LIMITS_POLL_INTERVAL`. The read itself is a token lookup plus
        // one HTTP call on a background thread (see `claude_usage`); the tick
        // only decides when. The interval is deliberately long - see the
        // constant - and a failing read does not shorten it.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(crate::app::agents_sidebar::LIMITS_FIRST_READ_DELAY).await;
                let mut why = crate::app::agents_sidebar::LimitsRefresh::Startup;
                loop {
                    if cx
                        .update(|cx| {
                            this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                                app.refresh_claude_limits(why, cx);
                                // The second vendor rides the same tick and is
                                // a file read rather than a request: see
                                // `refresh_codex_limits`.
                                app.refresh_codex_limits(cx);
                            })
                        })
                        .is_err()
                    {
                        break;
                    }
                    why = crate::app::agents_sidebar::LimitsRefresh::Tick;
                    // Asked of the wall clock once a minute rather than slept
                    // off in one timer, which does not advance while the
                    // machine sleeps - see `LIMITS_TICK_CHECK`.
                    loop {
                        smol::Timer::after(crate::app::agents_sidebar::LIMITS_TICK_CHECK).await;
                        match cx.update(|cx| {
                            this.update(cx, |app: &mut Self, _| app.claude_limits_tick_due())
                        }) {
                            Ok(true) => break,
                            Ok(false) => continue,
                            _ => return,
                        }
                    }
                }
            },
        )
        .detach();

        // Stale PID sweep: every 30s, probe registered AI agent PIDs with
        // kill(pid, 0) to detect crashed processes and clean up sidebar state.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                loop {
                    smol::Timer::after(std::time::Duration::from_secs(30)).await;
                    if cx
                        .update(|cx| {
                            this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                                app.sweep_stale_pids(cx);
                            })
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            },
        )
        .detach();

        // Port scanning and CWD detection are now event-driven:
        // - TerminalEvent::ActivityBurst → schedule_port_scan()
        // - TerminalEvent::CwdChanged → handle_cwd_change()
        // See handle_terminal_event() for the push-based implementation.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                loop {
                    smol::Timer::after(std::time::Duration::from_secs(5)).await;
                    if cx
                        .update(|cx| {
                            this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                                app.schedule_active_port_rescans(cx);
                            })
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            },
        )
        .detach();

        // Classify the install source once, then hand off to the
        // install-method hygiene migrations. Migrations are Linux-only and
        // the module itself is gated behind `#[cfg(target_os = "linux")]`,
        // so the call site needs the matching gate. On macOS / Windows the
        // tar.gz → rpm/deb crossover doesn't exist, so the helper isn't
        // compiled in at all.
        let install_method = update::install_method::detect();
        #[cfg(target_os = "linux")]
        update::migrations::run_startup_migrations(&install_method);

        // Compile-time env vars: the PostHog project key is injected by the
        // release pipeline; the host defaults to EU Cloud so a build that
        // omits the override still honours the EU data-residency constraint.
        let posthog_api_key = option_env!("POSTHOG_API_KEY").unwrap_or("");
        let posthog_host = option_env!("POSTHOG_HOST").unwrap_or("https://eu.i.posthog.com");
        let telemetry_config_snapshot = splitlane_config::loader::load_config();
        let telemetry_enabled_last = telemetry_config_snapshot
            .telemetry
            .as_ref()
            .and_then(|t| t.enabled);
        let telemetry_consent = telemetry::client::TelemetryConsent::new(telemetry_enabled_last);
        // Resolve the anonymous telemetry_id only after the
        // consent and kill-switch gates pass. Opt-out, unanswered consent, and
        // env kill-switches must not create persistent telemetry state.
        let (telemetry_distinct_id, is_first_run_for_telemetry) =
            if telemetry::client::TelemetryClient::consent_allows_capture(telemetry_consent) {
                telemetry::id::telemetry_id_with_first_run()
            } else {
                (String::new(), false)
            };
        let telemetry = std::sync::Arc::new(telemetry::client::TelemetryClient::from_consent(
            telemetry_consent,
            posthog_api_key,
            posthog_host,
            &telemetry_distinct_id,
        ));
        // One-shot boot warn when AI
        // free-access mode is enabled, mirroring the SPLITLANE_IPC_SCRIPTING
        // boot warn in `ipc::start_server()`. Reuses the snapshot already
        // loaded for telemetry so the file is not re-read. The fence is
        // independent and defaults ON, so it does not warn.
        if telemetry_config_snapshot.ai_unrestricted_enabled() {
            tracing::warn!(
                "ai.unrestricted is ON; same-UID callers may auto-submit prompts to agent panes without SPLITLANE_IPC_SCRIPTING (toggle in Settings -> AI Agent)"
            );
        }
        // Now that the telemetry client exists, fire off the
        // background update check. The detached worker emits
        // `update_check_started` immediately and `update_available`
        // only when both the version is greater AND an asset matched.
        // `check_for_updates: false` means the release feed is never contacted
        // at all - not checked and quietly ignored, but not reached. A locally
        // built binary looks identical to an official one to the updater, so
        // accepting an offered update would overwrite the user's own build.
        let pending_update = if telemetry_config_snapshot.check_for_updates == Some(false) {
            log::info!("update check disabled by config (check_for_updates: false)");
            update::checker::SharedUpdateSlot::default()
        } else {
            update::checker::spawn_check(
                std::sync::Arc::clone(&telemetry),
                update::checker::UpdateCheckTrigger::Auto,
            )
        };
        // Background flusher: every 5 s the client inspects its queue and
        // posts when the size or age threshold is met. Re-spawned when the
        // telemetry client is swapped by config reconciliation.
        Self::spawn_telemetry_flusher(std::sync::Arc::clone(&telemetry), cx);

        // Coexistence detection + one-time advisory toast. Runs
        // strictly after the icon migration so a same-session
        // upgrade→cleanup→toast chain stays in order. Detection is always
        // logged (the helper is still called for logging) so duplicate
        // installs remain visible in debug transcripts even after the
        // marker has muted the toast.
        #[cfg(target_os = "linux")]
        if let Some(report) = update::migrations::detect_coexistent_install(&install_method) {
            log::info!(
                "splitlane: coexistent install detected - running from {} (this install); other install at {} (installed via {})",
                report.running_path.display(),
                report.other_path.display(),
                report.other_method_label,
            );
            if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
                let marker_path = update::migrations::coexistence_marker_path(&home);
                if !marker_path.exists() {
                    // Build the toast payload up front so the spawn closure
                    // captures owned strings, not borrowed locals.
                    let message = format!(
                        "Two Splitlane installs detected. Running from {} (this install); other install at {} (installed via {}). Remove the unused install to avoid version drift.",
                        report.running_path.display(),
                        report.other_path.display(),
                        report.other_method_label,
                    );
                    let actions = vec![crate::ToastAction::OpenReleasesPage(
                        "https://github.com/ivkan/splitlane/blob/main/docs/user/installation.md"
                            .to_string(),
                    )];
                    let hold_ms = crate::TOAST_HOLD_MS * 4;
                    // `push_toast` needs `&mut Self` + `&mut Context<Self>`,
                    // but `Self` doesn't exist yet at this point in `new()`.
                    // Defer via `cx.spawn` - the first `Timer::after` yield
                    // lets the ctor finish and hands control back with a
                    // resolvable `WeakEntity<Self>`. Matches the established
                    // spawn pattern in this file (see git-watcher, port-scan,
                    // stale-PID sweep above).
                    cx.spawn(
                        async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                            smol::Timer::after(std::time::Duration::from_millis(1)).await;
                            let pushed = cx
                                .update(|cx| {
                                    this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                                        app.push_toast(message, actions, hold_ms, cx);
                                    })
                                })
                                .is_ok();
                            // Only persist the marker if the toast actually
                            // went out - a failed update means the app window
                            // is tearing down, in which case letting the toast
                            // recur next session is the right behaviour.
                            if pushed {
                                update::migrations::write_coexistence_marker(&marker_path);
                            }
                        },
                    )
                    .detach();
                }
            }
        }

        // The diff panel's persistent file filter. Observe it so each
        // keystroke re-renders the app (the TextInput only notifies itself).
        let diff_file_filter =
            cx.new(|cx| crate::widgets::text_input::TextInput::new("", "Filter files…", cx));
        cx.observe(&diff_file_filter, |_, _, cx| cx.notify())
            .detach();
        // Sessions sidebar search field. Same pattern, with one addition: the
        // observer mirrors the value into `sessions_query` (the row helpers take
        // `&self` and cannot read the entity) and re-clamps the keyboard
        // selection, which would otherwise point past the end of a list the
        // keystroke just shortened.
        let sessions_filter_input =
            cx.new(|cx| crate::widgets::text_input::TextInput::new("", "Search sessions", cx));
        cx.observe(&sessions_filter_input, |app, input, cx| {
            app.agent_sessions.sessions_query = input.read(cx).value().to_lowercase();
            app.clamp_sessions_selection();
            cx.notify();
        })
        .detach();
        // Codex settings nav search field - same pattern: a real single-line
        // TextInput, observed so each keystroke re-renders the nav to re-filter.
        let settings_search_input =
            cx.new(|cx| crate::widgets::text_input::TextInput::new("", "Search settings…", cx));
        cx.observe(&settings_search_input, |_, _, cx| cx.notify())
            .detach();
        // Keyboard Shortcuts page filter - same recipe, re-rendering the
        // grouped binding list on every keystroke.
        let shortcuts_filter_input =
            cx.new(|cx| crate::widgets::text_input::TextInput::new("", "Filter shortcuts…", cx));
        cx.observe(&shortcuts_filter_input, |_, _, cx| cx.notify())
            .detach();
        // The command palette query. Observed so each keystroke re-runs
        // the selection; the palette resets it to the top on every edit.
        let command_palette_query = cx.new(|cx| {
            crate::widgets::text_input::TextInput::new("", "Run a command or go to a surface…", cx)
        });
        cx.observe(&command_palette_query, |_, _, cx| cx.notify())
            .detach();

        let cached_config = splitlane_config::loader::load_config();
        let effective_shortcuts = keybindings::effective_shortcuts(&cached_config.shortcuts);
        let theme_mode = crate::ThemeMode::from_config(
            cached_config.theme_mode.as_deref(),
            cached_config.theme.as_deref(),
        );

        let mut app = Self {
            workspaces,
            active_idx,
            renaming_idx: None,
            rename_text: String::new(),
            pending_config,
            save_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            // Hydrate the render-path config cache once at startup.
            cached_config,
            ipc_rx,
            ipc_status,
            event_bus,
            last_broadcast_gen: std::collections::HashMap::new(),
            title_bar,
            primary_sidebar_visible: true,
            primary_sidebar_animation: None,
            git_watcher,
            git_event_rx,
            git_watch_counts,
            settings_section: None,
            settings_scroll: gpui::ScrollHandle::new(),
            settings_drag: None,
            settings_search_input,
            shortcuts_filter_input,
            terminal_dropdown: None,
            general_dropdown: None,
            appearance_theme_menu_open: false,
            presets: Vec::new(),
            mcp_status: None,
            mcp_install: None,
            mcp_busy: false,
            // `$HOME` is unset by default on Windows (canonical home is
            // `%USERPROFILE%`), so the raw `var("HOME")` produced an empty
            // string and the sidebar never collapsed any cwd to `~`. `dirs`
            // resolves the home dir on all three platforms.
            home_dir: dirs::home_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            sidebar_scroll: gpui::ScrollHandle::new(),
            effective_shortcuts,
            recording_shortcut_idx: None,
            settings_focus: cx.focus_handle(),
            mono_font_names: Vec::new(),
            font_dropdown_open: false,
            font_search: String::new(),
            theme_mode,
            workspace_menu_open: None,
            tab_menu_open: None,
            pending_pane_focus: None,
            pending_focus: None,
            pending_waiting_chip: false,
            ports_popover_open: false,
            restore_summary,
            agent_sessions: crate::AgentSessionsState {
                sessions_sidebar_open: false,
                sessions_sidebar_animation: None,
                sessions_by_agent: std::array::from_fn(|_| Vec::new()),
                sessions_omitted: [0; crate::agent_sessions::SESSION_AGENT_COUNT],
                sessions_cwd: None,
                sessions_surface_id: None,
                sessions_project_idx: None,
                sessions_scroll: gpui::ScrollHandle::new(),
                sessions_scan_generation: 0,
                sessions_selected: 0,
                sessions_filter_input: sessions_filter_input.clone(),
                sessions_query: String::new(),
                sessions_focus: cx.focus_handle(),
                sessions_group_collapsed: [false; crate::agent_sessions::SESSION_AGENT_COUNT],
                sessions_group_show_all: [false; crate::agent_sessions::SESSION_AGENT_COUNT],
                sessions_scanning: [false; crate::agent_sessions::SESSION_AGENT_COUNT],
            },
            files_sidebar_open: false,
            files_sidebar_animation: None,
            files_tree: crate::app::files_tree::FilesTreeState::default(),
            files_tree_scroll: gpui::ScrollHandle::new(),
            files_selected: 0,
            files_focus: cx.focus_handle(),
            empty_panes_focus: cx.focus_handle(),
            files_watcher: None,
            files_event_rx: None,
            files_menu_open: None,
            files_git_marks: std::collections::HashMap::new(),
            files_git_marks_generation: 0,
            toast: None,
            toast_queue: std::collections::VecDeque::new(),
            _toast_task: None,
            #[cfg(target_os = "windows")]
            windows_backdrop_light: None,
            jump_cursor: None,
            focused_pane_now: None,
            panes_area: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            dragging_rail_surface: None,
            pending_rail_surface_drop: None,
            pending_palette_output: None,
            focused_pane_before: None,
            swap_source: None,
            closed_panes: Vec::new(),
            show_about_dialog: false,
            show_theme_picker: false,
            theme_picker_query: String::new(),
            theme_picker_selected_idx: 0,
            theme_picker_focus: cx.focus_handle(),
            theme_picker_scroll: gpui::ScrollHandle::new(),
            theme_picker_drag: None,
            // Composer closed, no groups, no buffers.
            composer: None,
            broadcast: crate::app::broadcast::BroadcastState::default(),
            broadcast_picker_open: false,
            broadcast_picker_query: String::new(),
            broadcast_picker_selected: 0,
            broadcast_picker_renaming: None,
            broadcast_picker_error: None,
            broadcast_picker_focus: cx.focus_handle(),
            // Attention Queue + Launch Pad closed.
            attention_queue_open: false,
            attention_queue_selected: 0,
            attention_queue_focus: cx.focus_handle(),
            pending_exit: None,
            exit_confirm_focus: cx.focus_handle(),
            // Fleet grep closed.
            launch_pad: None,
            launch_pad_focus: cx.focus_handle(),
            worktree_dialog: None,
            worktree_dialog_focus: cx.focus_handle(),
            // Command palette closed.
            rail_width: restored_rail_width,
            files_width: restored_files_width,
            claude_limits: restored_limits,
            codex_limits: None,
            codex_limits_reading: false,
            files_resize_drag: None,
            rail_resize_drag: None,
            agent_state_reading: std::collections::HashSet::new(),
            worker_baselines: std::collections::HashMap::new(),
            proposed_sessions: std::collections::HashMap::new(),
            running_since: std::collections::HashMap::new(),
            run_end_seen_at: std::collections::HashMap::new(),
            activity_counts_cache: crate::app::waiting::ActivityCounts::default(),
            attention_queue_was_empty: false,
            attention_edge_needs_rebaseline: false,
            attention_edge_last_seen: None,
            announcement: None,
            announce_generation: 0,
            pty_flow: starting_baselines
                .iter()
                .map(|(id, generation)| {
                    (
                        *id,
                        crate::app::agent_state_pass::PtyFlow {
                            generation: *generation,
                            // `starting` is the launch's claim, not this
                            // source's; all that source does is take it down.
                            ours: false,
                        },
                    )
                })
                .collect(),
            composer_draft: None,
            command_palette: None,
            launcher_history: None,
            launcher_history_generation: 0,
            command_palette_focus: cx.focus_handle(),
            command_palette_query,
            command_palette_scroll: gpui::ScrollHandle::new(),
            self_update: crate::SelfUpdateState {
                pending_update,
                update_status: None,
                self_update_status: update::SelfUpdateStatus::default(),
                install_method,
                update_attempt_count: 0,
                download_generation: 0,
            },
            custom_buttons_modal: None,
            custom_buttons_modal_focus: cx.focus_handle(),
            telemetry,
            launch_instant: std::time::Instant::now(),
            telemetry_enabled_last,
            // Shared signal flipped by the theme watcher's debounce
            // thread; drained by the 50 ms IPC loop to schedule a repaint.
            theme_changed,
            diff_mode: crate::DiffModeState {
                diff_view: None,
                multi_diff_view: None,
                diff_view_cache: std::collections::HashMap::new(),
                diff_view_key: None,
                multi_diff_view_retained: None,
                diff_collapsed_branches: std::collections::HashSet::new(),
                diff_discovering: false,
                diff_discovering_root: None,
                diff_chosen_worktrees: std::collections::HashMap::new(),
                diff_worktree_picker_open: false,
                diff_available_worktrees: Vec::new(),
                diff_available_repo: None,
                diff_scope: restored_diff_scope,
                diff_scope_picker_open: false,
                diff_project_picker_open: false,
                diff_selected_file: None,
                diff_files_collapsed: false,
                diff_files_tree: false,
                diff_collapsed_dirs: std::collections::HashSet::new(),
                diff_file_filter,
                diff_full_window: None,
            },
            // Start in the mode the user
            // left on quit. The Agents view is terminal-only and works
            // without any agent installed, so there is no agent-presence
            // gate on restore.
            // Rehydrate project
            // metadata from session.json. Empty for users on first
            // launch and for legacy session.json (the `#[serde(default)]`
            // annotations make missing fields resolve to empty).
            // Restore the selected thread/chat when its stable ID still
            // exists after capping/filtering; otherwise start at picker/home.
            // Rename / context-menu / confirm-delete state.
            // All start empty; the affordance handlers set them in
            // response to user actions.
            agents_view: crate::AgentsViewState {
                agents_renaming: None,
                agents_rename_text: String::new(),
                agents_rename_input: None,
                agents_menu_open: None,
                agents_confirm_delete: None,
                agents_skills_tab: crate::agents_view::SkillsTab::default(),
                agents_skills: Vec::new(),
                agents_skills_loading: false,
                agents_skills_copied: None,
                sidebar_actions_menu_open: false,
                sidebar_mode_picker_open: false,
                agents_branch_menu: None,
                agents_environment_git: std::collections::HashMap::new(),
                // Agent surfaces restored into a slot are already live; the
                // cache has to know them before anything asks for one, or the
                // first request would build a second PTY for a surface that is
                // on screen - and the second one could not hold the session.
                agents_terminal_cache_lru: restored_agent_views
                    .iter()
                    .map(|(thread_id, _)| *thread_id)
                    .collect(),
                agents_terminal_cache_touched_at: restored_agent_views
                    .iter()
                    .map(|(thread_id, _)| (*thread_id, std::time::Instant::now()))
                    .collect(),
                agents_terminal_view_cache: restored_agent_views.into_iter().collect(),
            },
            // Sidebar search/filter. Empty filter == show
            // everything; the focus handle is held here so the input
            // captures Backspace/Escape/Down without conflicting with
            // the global app key chain.
            sidebar_order_cache: std::cell::RefCell::new(Default::default()),
            project_groups: restored_groups,
            pending_new_group: None,
            group_field_blur: None,
            rail_drag_kind: Default::default(),
            rail_group_focus: cx.focus_handle(),
            focused_rail_group: None,
        };

        for cwd in app
            .workspaces
            .iter()
            .map(|project| project.cwd.clone())
            .collect::<Vec<_>>()
        {
            app.spawn_agents_environment_git_refresh(cwd, cx);
        }
        if let Some(target) = app.current_thread_view_target(cx) {
            app.mount_agents_terminal_for_target(target, cx);
        }
        // A diff restored into a pane still needs its app-side host mounted,
        // or the surface comes back reading "No Git repository" over a repo
        // that is right there.
        if app.active_container_shows_a_diff(cx) {
            app.rebuild_diff_view(cx);
        }

        // Fire `app_started` once per launch. `Null` clients
        // (opt-out / unanswered consent / env kill-switch) no-op; only a
        // consenting user produces an HTTP call, batched on the flusher
        // above. Must happen after the struct literal so `self.telemetry`
        // and `self.self_update.install_method` are both populated.
        // Said out loud, because the container on screen is not the one the
        // file described. Two reasons it can differ, and each says its own,
        // because "where did my shell go" has a different answer in each: a
        // build with a higher pane cap wrote the file and the slots past the
        // cap came back as rail rows, or a build whose panes had tab strips
        // wrote it and the background tabs did.
        //
        // One line each, and the toast ellipsizes past ~340px - so each says
        // the two facts that matter (what fired; the shells are in the rail)
        // and nothing else.
        // Held far longer than a routine toast: these fire while the window is
        // still coming up, and the default 1.4s is gone before the user has
        // finished looking at the screen they describe.
        let shells = |n: usize| {
            if n == 1 {
                "1 shell".to_string()
            } else {
                format!("{n} shells")
            }
        };
        if parked_over_cap > 0 {
            app.push_toast(
                format!(
                    "Pane cap {}: {} moved to the sidebar",
                    crate::layout::MAX_PANES,
                    shells(parked_over_cap)
                ),
                Vec::new(),
                crate::app::constants::TOAST_HOLD_MS * 8,
                cx,
            );
        }
        if parked_extra_tabs > 0 {
            app.push_toast(
                format!(
                    "One surface per pane: {} to the sidebar",
                    shells(parked_extra_tabs)
                ),
                Vec::new(),
                crate::app::constants::TOAST_HOLD_MS * 8,
                cx,
            );
        }

        // The one-time move of `commands[].workspace` into the presets
        // folder. It runs at launch, once, and only when the old key is
        // non-empty and the folder is empty; after this release nothing reads
        // that key, so a user who has presets would otherwise find them gone
        // with no way to know they had existed. "Recreate it in a minute" is
        // only true if you can see what to recreate.
        //
        // Off the render thread: it reads and writes a folder. The key stays
        // in `splitlane.json`, readable; there is no converter back.
        let legacy_presets = app.cached_config.commands.clone();
        if legacy_presets.iter().any(|c| c.workspace.is_some()) {
            cx.spawn(async move |this, cx| {
                let written = smol::unblock(move || {
                    crate::preset::store::migrate_from_commands(&legacy_presets)
                })
                .await;
                if written.is_empty() {
                    return;
                }
                let _ = this.update(cx, |app, cx| {
                    let count = written.len();
                    let noun = if count == 1 { "preset" } else { "presets" };
                    app.push_toast(
                        format!("{count} {noun} moved to the presets folder"),
                        Vec::new(),
                        crate::app::constants::TOAST_HOLD_MS * 6,
                        cx,
                    );
                    // The cache read below started at the same moment and may
                    // have finished first, on an empty folder. Read it again
                    // now that the files exist, or the project row's
                    // "Run preset" is missing for the whole first launch.
                    app.reload_presets(cx);
                });
            })
            .detach();
        }

        // Fill the preset cache. Deferred rather than blocking the first
        // frame; every reader of it renders an empty list until it lands.
        app.reload_presets(cx);

        app.emit_app_started(is_first_run_for_telemetry);
        // Emit the corruption event after the client is up.
        // `Null` clients (consent off / kill-switch active) make this
        // a no-op without a network call.
        if let Some(info) = session_corruption {
            app.emit_session_corrupted(&info);
        }

        // Memory: opportunistically release exited cached agent
        // terminals after their idle TTL even if the user never selects another
        // thread. Running PTYs stay protected by the eviction guard.
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                loop {
                    smol::Timer::after(std::time::Duration::from_secs(60)).await;
                    let result = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                            // The surface in the focused pane is the one the
                            // user is looking at, and the eviction budget must
                            // never drop its view out from under them.
                            let active_thread_id =
                                app.current_thread_view_target(cx).and_then(|target| {
                                    app.thread_for_target(target).map(|thread| thread.id)
                                });
                            app.enforce_agents_terminal_cache_budget(active_thread_id, cx);
                        })
                    });
                    if result.is_err() {
                        break;
                    }
                }
            },
        )
        .detach();

        // The slot header's model badge: `Thread::model` has no other source,
        // so it is read from each visible agent surface's own transcript.
        app.spawn_model_probe(cx);
        // What every agent surface is doing, from the file it writes for itself
        // plus the liveness of its process. The hook stream is an accelerator
        // over this, not the source - see `app::agent_state_pass`.
        app.spawn_agent_state_pass(cx);

        app
    }
}

// ---------------------------------------------------------------------------
// Free helper functions called from `fn main()`.
// ---------------------------------------------------------------------------

/// Build the copy-pasteable upgrade command for a system-package install.
///
/// `version` is safe to interpolate into a shell string without escaping: it
/// comes from `UpdateStatus::Available { version }`, which is set from a
/// `semver::Version::to_string()` - the semver parser rejects any input that
/// would survive into `;`/`$()`/whitespace/bidi, so malformed GitHub tags
/// short-circuit to `UpdateStatus::Failed` long before this function runs.
///
/// Version format notes:
/// - apt pinning uses `name=upstream-debrev`. `cargo-deb` emits `-1` as the
///   debian revision by default, so `splitlane=<v>-1` targets the exact tag.
/// - dnf accepts `name-upstream` as a NEVR prefix match. The `<v>` we pass is
///   already the raw upstream version from GitHub Releases.
/// - zypper accepts `name=upstream` for exact version selection.
/// - `PackageManager::Other` gets a plain-English hint rather than a command,
///   because we don't know the syntax (eopkg/xbps/apk all differ).
pub(crate) fn system_package_update_command(
    manager: Option<&update::install_method::PackageManager>,
    version: &str,
) -> String {
    match manager {
        Some(update::install_method::PackageManager::Apt) => {
            format!("sudo apt update && sudo apt install splitlane={version}-1")
        }
        Some(update::install_method::PackageManager::Dnf) => {
            // Match the validated pkexec argv (`dnf --refresh install …`): plain
            // `dnf upgrade <pkg>-<ver>` hits "No match for argument" right after
            // a publish because the cached metadata predates the new version.
            // `--refresh install` forces a metadata sync and installs the exact
            // version (dnf treats install of a higher version as an upgrade).
            format!("sudo dnf --refresh install splitlane-{version}")
        }
        Some(update::install_method::PackageManager::Zypper) => {
            format!(
                "sudo zypper --non-interactive --gpg-auto-import-keys refresh && sudo zypper --non-interactive install --no-recommends --force splitlane={version}"
            )
        }
        // `rpm-ostree upgrade` takes no package argument - it
        // rebases the whole deployment. Version string is intentionally
        // NOT included, unlike the apt/dnf arms.
        Some(update::install_method::PackageManager::RpmOstree) => "rpm-ostree upgrade".to_string(),
        Some(update::install_method::PackageManager::Other) | None => {
            "Update Splitlane via your system's package manager".to_string()
        }
    }
}

/// Install the macOS menu bar.
///
/// Three top-level menus - Splitlane / Edit / Window - populated with
/// the actions below. The `Splitlane` menu name matches the
/// `CFBundleName` in the bundle's Info.plist. Keyboard shortcuts
/// are derived from the global keybindings table (e.g. Quit shows `⌘Q`
/// because `MACOS_ONLY_DEFAULTS` binds `cmd-q → quit`; Window items
/// show `⌘⇧N` / `⌘⇧Q` / `⌘Tab` from the `secondary-*` bindings).
/// Copy / Paste / Select All carry an `OsAction` hint so macOS routes them
/// through the native responder chain and renders `⌘C` / `⌘V` / `⌘A`.
#[cfg(target_os = "macos")]
pub(crate) fn install_macos_menu_bar(cx: &mut gpui::App) {
    use gpui::{Menu, MenuItem, OsAction};

    use crate::{About, CloseWorkspace, Copy, NewWorkspace, NextWorkspace, OpenHelp, Paste, Quit};

    cx.set_menus(vec![
        Menu::new("Splitlane").items(vec![
            MenuItem::action("About Splitlane", About),
            MenuItem::separator(),
            MenuItem::action("Quit Splitlane", Quit),
        ]),
        // No "Select All": the terminal has no select-all, so the entry was
        // an enabled menu item wired to a logging stub. It comes back with
        // the feature, not before it.
        Menu::new("Edit").items(vec![
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
        ]),
        Menu::new("Window").items(vec![
            // The rail says PROJECTS and so does every other label; the action
            // *names* stay `new_workspace` etc. because they are a config
            // surface - a user's `shortcuts` map keys onto them - and renaming
            // one would silently unbind their override.
            MenuItem::action("New Project", NewWorkspace),
            MenuItem::action("Close Project", CloseWorkspace),
            MenuItem::separator(),
            MenuItem::action("Next Project", NextWorkspace),
        ]),
        // macOS convention: every app ships a Help menu (even if it only
        // points to an online doc/repo). Without one, Apple's HIG-conforming
        // users perceive the app as unfinished. "Splitlane Help" dispatches
        // `OpenHelp` which opens the GitHub README in the default browser.
        Menu::new("Help").items(vec![MenuItem::action("Splitlane Help", OpenHelp)]),
    ]);
}

/// Register macOS menu actions as app-global fallbacks.
///
/// AppKit validates menu items via GPUI's `is_action_available`, which checks
/// the focused dispatch path plus app-global listeners. Splitlane's normal
/// handlers live on the rendered root element so keyboard/menu dispatch works
/// while that root is in the focused path, but macOS can validate the native
/// menu while focus sits in a terminal/Agents surface whose current rendered
/// path does not expose the root listeners. These fallbacks make the native
/// menu items consistently enabled and mirror the root handlers when they are
/// otherwise unreachable.
#[cfg(target_os = "macos")]
pub(crate) fn install_macos_menu_action_fallbacks(cx: &mut gpui::App) {
    use crate::{
        About, CloseWorkspace, Copy, NewWorkspace, NextWorkspace, OpenHelp, Paste, Quit,
        SplitlaneApp,
    };

    fn with_active_splitlane_window(
        cx: &mut gpui::App,
        f: impl FnOnce(&mut SplitlaneApp, &mut gpui::Window, &mut Context<SplitlaneApp>),
    ) {
        let Some(window) = cx.active_window() else {
            return;
        };
        let Some(window) = window.downcast::<SplitlaneApp>() else {
            return;
        };
        if let Err(err) = window.update(cx, f) {
            log::debug!("macOS menu fallback: active Splitlane window unavailable: {err}");
        }
    }

    cx.on_action(|_: &Quit, cx| {
        with_active_splitlane_window(cx, |app, _window, cx| {
            app.request_exit(crate::app::exit_guard::ExitIntent::Quit, cx);
        });
    });

    cx.on_action(|_: &About, cx| {
        with_active_splitlane_window(cx, |app, _window, cx| {
            app.show_about_dialog = true;
            cx.notify();
        });
    });

    cx.on_action(|_: &Copy, cx| route_os_copy(cx));
    cx.on_action(|_: &Paste, cx| route_os_paste(cx));

    cx.on_action(|_: &NewWorkspace, cx| {
        with_active_splitlane_window(cx, |app, window, cx| {
            app.create_workspace_with_picker(window, cx);
        });
    });
    cx.on_action(|_: &CloseWorkspace, cx| {
        with_active_splitlane_window(cx, |app, window, cx| {
            app.close_workspace_at(app.active_idx, window, cx);
        });
    });
    cx.on_action(|_: &NextWorkspace, cx| {
        with_active_splitlane_window(cx, |app, window, cx| {
            if !app.workspaces.is_empty() {
                let next = (app.active_idx + 1) % app.workspaces.len();
                app.select_workspace(next, window, cx);
            }
        });
    });

    cx.on_action(|_: &OpenHelp, cx| {
        with_active_splitlane_window(cx, |app, _window, cx| {
            if let Err(e) =
                crate::external_open::open_url("https://github.com/ivkan/splitlane#readme")
            {
                log::warn!("Help > Splitlane Help: could not open browser: {e}");
                app.show_toast(format!("Could not open help: {e}"), cx);
            }
        });
    });
}

/// The candidates `\u{2318}C` is offered to, nearest surface first.
///
/// Order is the whole content of the rule, which is why it is a function with
/// a test rather than a literal at each call site: a text field, then a
/// rendered document, then the terminal - the terminal last because it is the
/// fallback, not the default.
fn copy_candidates() -> Vec<Box<dyn gpui::Action>> {
    use crate::MarkdownCopy;
    use crate::widgets::text_area::TaCopy;
    use crate::widgets::text_input::TextInputCopy;
    vec![
        Box::new(TaCopy),
        Box::new(TextInputCopy),
        Box::new(MarkdownCopy),
        Box::new(crate::TerminalCopy),
    ]
}

/// The same ladder for `\u{2318}V`. A document has nothing to paste into, so it
/// is not on this one.
fn paste_candidates() -> Vec<Box<dyn gpui::Action>> {
    use crate::widgets::text_area::TaPaste;
    use crate::widgets::text_input::TextInputPaste;
    vec![
        Box::new(TaPaste),
        Box::new(TextInputPaste),
        Box::new(crate::TerminalPaste),
    ]
}

/// Hand the action to the first candidate the focused element of **this**
/// window can actually answer.
///
/// `is_action_available` asks GPUI's dispatch tree along the path to the
/// focused node, which is the same question dispatch itself asks - so a
/// candidate that answers `true` here is one whose handler will run, and
/// nothing is dispatched into the void.
///
/// The window is a parameter rather than something re-derived from
/// `active_window()`, because the caller that has one is the caller that
/// received the action: asking the app which window is active is a second
/// answer to a question already settled, and the two can differ.
fn dispatch_to_focused_in(
    window: &mut gpui::Window,
    cx: &mut gpui::App,
    candidates: Vec<Box<dyn gpui::Action>>,
) {
    for action in candidates {
        if window.is_action_available(action.as_ref(), cx) {
            window.dispatch_action(action.boxed_clone(), cx);
            return;
        }
    }
}

/// The same, for the app-global fallback, which has no window of its own.
///
/// `App::is_action_available` and `App::dispatch_action` both resolve through
/// `active_window()`, so they agree with each other; this is only ever reached
/// when AppKit validates or fires the menu item while the rendered root's own
/// listener is not on the focused path.
/// macOS-only: its sole callers are `route_os_copy` / `route_os_paste`, which
/// exist for the macOS Edit menu. The window-level `*_in` twins below are
/// cross-platform and stay ungated.
#[cfg(target_os = "macos")]
fn dispatch_to_focused(cx: &mut gpui::App, candidates: Vec<Box<dyn gpui::Action>>) {
    for action in candidates {
        if cx.is_action_available(action.as_ref()) {
            cx.dispatch_action(action.as_ref());
            return;
        }
    }
}

/// macOS Edit -> Copy, routed to whatever has focus.
///
/// The menu item carries `OsAction::Copy`, so AppKit gives it the `\u{2318}C` key
/// equivalent and `performKeyEquivalent:` claims the chord **before** the
/// window sees a key press. Every `\u{2318}C` in the app therefore arrives here and
/// nowhere else: the `TextInput` / `SplitlaneTextArea` bindings never fire, and
/// neither does `MACOS_ONLY_DEFAULTS`' `cmd-c` -> `markdown_copy`, whose comment
/// used to say nothing native competed for it.
///
/// Both handlers sent it straight to the terminal, which is right for the one
/// surface that is usually focused and wrong everywhere else - reported from
/// live use as "renaming a session does not support paste".
#[cfg(target_os = "macos")]
pub(crate) fn route_os_copy(cx: &mut gpui::App) {
    dispatch_to_focused(cx, copy_candidates());
}

/// The window-level door: the rendered root's own `Copy` listener.
pub(crate) fn route_os_copy_in(window: &mut gpui::Window, cx: &mut gpui::App) {
    dispatch_to_focused_in(window, cx, copy_candidates());
}

/// macOS Edit -> Paste, routed the same way. See [`route_os_copy`].
#[cfg(target_os = "macos")]
pub(crate) fn route_os_paste(cx: &mut gpui::App) {
    dispatch_to_focused(cx, paste_candidates());
}

/// The window-level door: the rendered root's own `Paste` listener.
pub(crate) fn route_os_paste_in(window: &mut gpui::Window, cx: &mut gpui::App) {
    dispatch_to_focused_in(window, cx, paste_candidates());
}

/// The old `.run` installer (since removed) dropped a standalone binary
/// at `~/.local/bin/splitlane`. The new tar.gz installer instead drops a
/// `~/.local/splitlane.app/` directory and symlinks `~/.local/bin/splitlane`
/// into it. We warn when the old layout is detected so users know why the
/// in-app updater can no longer fetch a `.run` asset (there are none).
pub(crate) fn warn_if_legacy_run_install() {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return;
    };
    let app_dir = home.join(".local/splitlane.app");
    let legacy_bin = home.join(".local/bin/splitlane");

    let legacy_bin_is_regular_file = legacy_bin
        .symlink_metadata()
        .map(|m| m.file_type().is_file())
        .unwrap_or(false);

    if !app_dir.exists() && legacy_bin_is_regular_file {
        log::warn!(
            "legacy .run install detected at {} - see README for migration \
             to the .tar.gz / .deb / .AppImage formats",
            legacy_bin.display()
        );
    }
}

/// Detect whether the Apple Silicon binary is running under Rosetta 2
/// translation on an Intel Mac (or, more commonly, an Intel binary on
/// Apple Silicon - which Apple translates transparently). Either way it
/// warns once at startup so a user who grabbed the wrong `.dmg` knows
/// why GPU performance is degraded instead of silently eating the hit.
///
/// Uses `sysctl.proc_translated`: returns
/// `1` for a translated process, `0` native, ENOENT → native Intel kernel
/// (no Rosetta available at all). Failure to read the sysctl is silent -
/// this warning is diagnostic, not load-bearing.
#[cfg(target_os = "macos")]
pub(crate) fn warn_if_rosetta_translated() {
    use std::ffi::CString;
    use std::mem::size_of;

    let name = match CString::new("sysctl.proc_translated") {
        Ok(n) => n,
        Err(_) => return,
    };
    let mut translated: i32 = 0;
    let mut size = size_of::<i32>();
    // SAFETY: `sysctlbyname` reads a small integer into a stack buffer whose
    // size is passed by pointer. `name.as_ptr()` is a valid NUL-terminated
    // C string from a CString we just constructed. `translated` and `size`
    // are live stack variables for the duration of the call. Zero-initialized
    // buffer means a kernel short-write can't expose uninitialized memory.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut translated as *mut _ as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && translated == 1 {
        log::warn!(
            "running under Rosetta 2 translation - GPU rendering will be \
             degraded. For best performance, download the matching \
             architecture from https://github.com/ivkan/splitlane/releases"
        );
    }
}

#[cfg(test)]
mod clipboard_routing_tests {
    use super::{copy_candidates, paste_candidates};

    /// The ladder's shape, pinned whole rather than at its ends. Nothing else
    /// observes it: a wrong order still compiles, still dispatches, and still
    /// looks right in a terminal - which is the one surface that would keep
    /// working. The middle matters as much as the ends, because a text input
    /// and a text area are never both on a focus path and a reorder between
    /// them would be invisible until the day one nests inside the other.
    #[test]
    fn the_terminal_is_the_last_thing_asked() {
        let names = |ladder: Vec<Box<dyn gpui::Action>>| {
            ladder
                .iter()
                .map(|action| action.name().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(copy_candidates()),
            [
                "splitlane_text_area::TaCopy",
                "text_input::TextInputCopy",
                "splitlane::MarkdownCopy",
                "splitlane::TerminalCopy",
            ]
        );
        assert_eq!(
            names(paste_candidates()),
            [
                "splitlane_text_area::TaPaste",
                "text_input::TextInputPaste",
                "splitlane::TerminalPaste",
            ]
        );
    }

    /// The production path, which is the menu's and not the keyboard's: on
    /// macOS the chord never reaches the window, so a test that simulates the
    /// keystroke exercises the one platform where the bug was not.
    ///
    /// Drives `route_os_paste_in` itself, with a real focused field, and
    /// asserts the text landed - which is the whole of what the fix promises.
    /// The app-level twin cannot be reached from here: GPUI's test platform
    /// reports no active window, which is exactly what that twin resolves
    /// through - so this covers the door that has a window, which is the one
    /// the rendered root uses.
    #[gpui::test]
    fn the_menus_paste_reaches_the_focused_field(cx: &mut gpui::TestAppContext) {
        use crate::widgets::text_area::TextArea;

        cx.update(crate::widgets::text_area::register_keybindings);
        let (view, cx) = cx.add_window_view(|_, cx| TextArea::new("New name", cx));
        cx.simulate_resize(gpui::size(gpui::px(300.), gpui::px(60.)));
        let handle = cx.update(|_, cx| view.read(cx).focus_handle.clone());
        cx.update(|window, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("from the menu".to_string()));
            window.focus(&handle, cx);
            window.draw(cx).clear();
        });
        cx.update(super::route_os_paste_in);
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert_eq!(
            view.read_with(cx, |area, _| area.value()),
            "from the menu",
            "Edit > Paste did not reach the focused field"
        );
    }
}
