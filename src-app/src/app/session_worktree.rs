//! A worktree of its own for every agent session a person starts.
//!
//! Deciding before a session whether it will touch code is work nobody can do
//! well: a session opened to talk over a meeting ends up editing files, and
//! one opened to fix a bug ends up a discussion. So a session a person starts
//! in a git project gets its own worktree and branch, `splitlane/<four>`,
//! cut from the project's current `HEAD` - about a third of a second of git,
//! which runs before the agent starts so the agent starts in it. A project
//! whose work goes straight to its main branch turns this off once
//! ([`crate::workspace::Workspace::session_worktrees`]).
//!
//! The worktree is the session's: when the session is closed and the
//! worktree holds nothing anybody would miss, it goes, and its branch with
//! it if nothing was committed on it. A worktree with changes, or a branch
//! with commits, stays, and the person is told so. Sessions another session
//! opens work in their opener's worktree rather than getting one each.

use gpui::Context;

use crate::SplitlaneApp;
use crate::workspace::worktree::{self, ManagedWorktree, Teardown};

/// How long a closed session's agent is given to exit before its worktree is
/// examined. Removing a directory a process still writes in would race it.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

impl SplitlaneApp {
    /// Whether a session a person starts in `ws_idx` gets its own worktree.
    pub(crate) fn sessions_get_worktrees(&self, ws_idx: usize) -> bool {
        self.workspaces
            .get(ws_idx)
            .is_some_and(|ws| ws.repo_root.is_some() && ws.session_worktrees.unwrap_or(true))
    }

    /// Turn the project's own-worktree-per-session choice over.
    pub(crate) fn toggle_session_worktrees(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        let on = self.sessions_get_worktrees(ws_idx);
        if let Some(ws) = self.workspaces.get_mut(ws_idx) {
            ws.session_worktrees = Some(!on);
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Start the just-created session `thread_id` in a worktree of its own,
    /// when its project gives sessions one. Returns whether it will: the
    /// caller then leaves selecting the session to the completion, because
    /// its terminal cannot start before the worktree exists.
    pub(crate) fn give_session_own_worktree(
        &mut self,
        ws_idx: usize,
        thread_id: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sessions_get_worktrees(ws_idx) {
            return false;
        }
        let Some(ws) = self.workspaces.get_mut(ws_idx) else {
            return false;
        };
        let Some(repo_root) = ws.repo_root.clone() else {
            return false;
        };
        let Some(thread) = ws.threads.iter_mut().find(|t| t.id == thread_id) else {
            return false;
        };
        thread.worktree_pending = true;
        let container_id = ws.id;
        let setup = ws.worktree_setup.clone().unwrap_or_default();
        let held: Vec<u16> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.managed_worktrees.iter().filter_map(|wt| wt.port))
            .collect();
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let made = smol::unblock(move || {
                    let mut made = worktree::add_session_worktree(&repo_root)?;
                    // Best-effort by design: a partial copy is not a failure.
                    worktree::copy_local_files(&repo_root, &made.path);
                    made.port = worktree::allocate_port(&held);
                    Ok::<ManagedWorktree, String>(made)
                })
                .await;
                let started = cx
                    .update(|cx| {
                        this.update(cx, |app, cx| {
                            app.session_worktree_made(container_id, thread_id, made, cx)
                        })
                    })
                    .ok()
                    .flatten();
                // The setup command runs beside the agent rather than in
                // front of it: the session is usable at once, and an agent
                // that needs the install can wait for it.
                if let Some(made) = started.filter(|_| !setup.trim().is_empty()) {
                    let (path, port, branch) = (made.path.clone(), made.port, made.branch);
                    let failed = smol::unblock(move || {
                        worktree::run_setup_with_port(
                            &path,
                            port,
                            &setup,
                            worktree::DEFAULT_SETUP_TIMEOUT,
                        )
                        .err()
                    })
                    .await;
                    if let Some(error) = failed {
                        let _ = cx.update(|cx| {
                            this.update(cx, |app, cx| {
                                app.show_toast(format!("Setup in {branch} failed: {error}"), cx);
                            })
                        });
                    }
                }
            },
        )
        .detach();
        true
    }

    /// The worktree for a new session is made, or could not be. Either way the
    /// session starts now: in the worktree, or in the project folder with a
    /// word about why. Returns the worktree when the session went into it.
    fn session_worktree_made(
        &mut self,
        container_id: u64,
        thread_id: u64,
        made: Result<ManagedWorktree, String>,
        cx: &mut Context<Self>,
    ) -> Option<ManagedWorktree> {
        let ws_idx = self.workspaces.iter().position(|ws| ws.id == container_id);
        let thread_idx = ws_idx.and_then(|ws_idx| {
            self.workspaces[ws_idx]
                .threads
                .iter()
                .position(|t| t.id == thread_id)
        });
        let (Some(ws_idx), Some(thread_idx)) = (ws_idx, thread_idx) else {
            // Closed while git ran: the worktree has nobody, and nothing is
            // in it yet.
            if let Ok(made) = made {
                cx.background_executor()
                    .spawn(async move {
                        worktree::teardown_one(&made, &[]);
                    })
                    .detach();
            }
            return None;
        };
        let thread = &mut self.workspaces[ws_idx].threads[thread_idx];
        thread.worktree_pending = false;
        let started = match made {
            Ok(made) => {
                thread.cwd = made.path.to_string_lossy().into_owned();
                self.workspaces[ws_idx].managed_worktrees.push(made.clone());
                Some(made)
            }
            Err(error) => {
                self.show_toast(
                    format!("No branch for this session ({error}); it runs in the project folder"),
                    cx,
                );
                None
            }
        };
        // Recorded before anything else can fail, so a quit now does not
        // cost the worktree its owner.
        self.save_session(cx);
        let _ = self.select_thread(ws_idx, thread_idx, cx);
        cx.notify();
        started
    }

    /// Where a session another session opened works: in its opener's
    /// worktree, when the opener has one, rather than in one of its own.
    pub(crate) fn inherit_opener_worktree(
        &mut self,
        ws_idx: usize,
        thread_id: u64,
        opener_id: u64,
    ) {
        let Some(ws) = self.workspaces.get_mut(ws_idx) else {
            return;
        };
        let Some(opener_cwd) = ws
            .threads
            .iter()
            .find(|t| t.id == opener_id)
            .map(|t| t.cwd.clone())
        else {
            return;
        };
        let in_worktree = ws
            .managed_worktrees
            .iter()
            .any(|wt| std::path::Path::new(&opener_cwd).starts_with(&wt.path));
        if in_worktree && let Some(thread) = ws.threads.iter_mut().find(|t| t.id == thread_id) {
            thread.cwd = opener_cwd;
        }
    }

    /// A session was closed. If it was the last one working in a worktree made
    /// for a session, that worktree is torn down once the agent has had a
    /// moment to exit, and the person hears what stayed.
    pub(crate) fn release_session_worktree(
        &mut self,
        ws_idx: usize,
        closed_cwd: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspaces.get_mut(ws_idx) else {
            return;
        };
        let closed = std::path::Path::new(closed_cwd);
        let Some(at) = ws
            .managed_worktrees
            .iter()
            .position(|wt| wt.base.is_some() && closed.starts_with(&wt.path))
        else {
            return;
        };
        let path = ws.managed_worktrees[at].path.clone();
        if ws
            .threads
            .iter()
            .any(|t| std::path::Path::new(&t.cwd).starts_with(&path))
        {
            return;
        }
        let released = ws.managed_worktrees.remove(at);
        let container_id = ws.id;
        let open = self.container_paths_outliving(usize::MAX);
        self.save_session(cx);
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(CLOSE_GRACE).await;
                let examined = released.clone();
                let outcome = smol::unblock(move || worktree::teardown_one(&examined, &open)).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        app.session_worktree_released(container_id, released, outcome, cx);
                    })
                });
            },
        )
        .detach();
    }

    fn session_worktree_released(
        &mut self,
        container_id: u64,
        released: ManagedWorktree,
        outcome: Teardown,
        cx: &mut Context<Self>,
    ) {
        let branch = released.branch.clone();
        match outcome {
            Teardown::Removed {
                branch_deleted: true,
            } => {}
            Teardown::Removed {
                branch_deleted: false,
            } => {
                self.show_toast(format!("Branch {branch} kept: it has commits"), cx);
            }
            Teardown::Kept(reason) => {
                self.show_toast(
                    format!(
                        "Worktree for {branch} kept: {reason} - {}",
                        released.path.display()
                    ),
                    cx,
                );
                // Still the project's, so its close tries again.
                if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == container_id) {
                    ws.managed_worktrees.push(released);
                    self.save_session(cx);
                }
            }
        }
    }
}
