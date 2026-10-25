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
use crate::workspace::session_branch::{self, BranchWork};
use crate::workspace::worktree::{self, ManagedWorktree, Teardown};

/// How long a closed session's agent is given to exit before its worktree is
/// examined. Removing a directory a process still writes in would race it.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

/// How long a new session's worktree is made in silence before its row says
/// `making branch…`. The usual third of a second passes unremarked.
pub(crate) const MAKING_BRANCH_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

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
        thread.worktree_pending = Some(std::time::Instant::now());
        thread.worktree_failed = None;
        let container_id = ws.id;
        let setup = ws.worktree_setup.clone().unwrap_or_default();
        let from = (!ws.git_branch.is_empty()).then(|| ws.git_branch.clone());
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
                    made.from = from;
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
        // Past half a second the row says what it is waiting for; this is
        // the frame that makes it look.
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(MAKING_BRANCH_AFTER).await;
                let _ = cx.update(|cx| this.update(cx, |_, cx| cx.notify()));
            },
        )
        .detach();
        true
    }

    /// The `no branch` row's way out: start the session in the project
    /// folder, as a person chose to.
    pub(crate) fn open_session_without_branch(&mut self, thread_id: u64, cx: &mut Context<Self>) {
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|ws| ws.threads.iter().any(|t| t.id == thread_id))
        else {
            return;
        };
        if let Some(thread) = self.workspaces[ws_idx]
            .threads
            .iter_mut()
            .find(|t| t.id == thread_id)
        {
            thread.worktree_failed = None;
        }
        self.select_session(ws_idx, thread_id, cx);
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
        let Some(ws_idx) = self.workspaces.iter().position(|ws| ws.id == container_id) else {
            // The project closed while git ran. Its close tore down what it
            // owned, and this was not among it yet.
            self.phone_starts.take_awaiting(thread_id);
            if let Ok(made) = made {
                cx.background_executor()
                    .spawn(async move {
                        if let Teardown::Kept(reason) = worktree::teardown_one(&made, &[]) {
                            log::warn!(
                                "session worktree left at {} ({reason}): its project closed",
                                made.path.display()
                            );
                        }
                    })
                    .detach();
            }
            return None;
        };
        let mut made = match made {
            Ok(made) => made,
            Err(error) => {
                if let Some(prompt) = self.phone_starts.take_awaiting(thread_id) {
                    self.start_phone_session_without_branch(thread_id, prompt, &error, cx);
                    return None;
                }
                if let Some(thread) = self.workspaces[ws_idx]
                    .threads
                    .iter_mut()
                    .find(|t| t.id == thread_id)
                {
                    thread.worktree_pending = None;
                    thread.worktree_failed = Some(error);
                }
                cx.notify();
                return None;
            }
        };
        // Two sessions started together each chose a port before either was
        // recorded; the second one to land takes another block.
        let held: Vec<u16> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.managed_worktrees.iter().filter_map(|wt| wt.port))
            .collect();
        if made
            .port
            .is_some_and(|port| worktree::port_block_overlaps(port, &held))
        {
            made.port = worktree::allocate_port(&held);
        }
        // Recorded before anything else can fail, so a quit now does not
        // cost the worktree its owner.
        self.workspaces[ws_idx].managed_worktrees.push(made.clone());
        let Some(thread) = self.workspaces[ws_idx]
            .threads
            .iter_mut()
            .find(|t| t.id == thread_id)
        else {
            // The session was closed while git ran: its worktree goes the way
            // a closed session's does.
            self.phone_starts.take_awaiting(thread_id);
            self.save_session(cx);
            self.spawn_session_worktree_release(container_id, made, cx);
            return None;
        };
        thread.worktree_pending = None;
        thread.cwd = made.path.to_string_lossy().into_owned();
        self.save_session(cx);
        // A session a phone started starts where it is, in the rail: the
        // person at the computer did not ask for it to come on screen.
        match self.phone_starts.take_awaiting(thread_id) {
            Some(prompt) => {
                self.start_phone_session_or_drop(thread_id, prompt, cx);
            }
            None => self.select_session(ws_idx, thread_id, cx),
        }
        Some(made)
    }

    fn select_session(&mut self, ws_idx: usize, thread_id: u64, cx: &mut Context<Self>) {
        if let Some(thread_idx) = self.thread_index_by_id(ws_idx, thread_id) {
            let _ = self.select_thread(ws_idx, thread_idx, cx);
        }
        cx.notify();
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
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return;
        };
        let closed = std::path::Path::new(closed_cwd);
        let Some(released) = ws
            .managed_worktrees
            .iter()
            .find(|wt| wt.base.is_some() && closed.starts_with(&wt.path))
            .cloned()
        else {
            return;
        };
        if ws
            .threads
            .iter()
            .any(|t| std::path::Path::new(&t.cwd).starts_with(&released.path))
        {
            return;
        }
        let container_id = ws.id;
        self.spawn_session_worktree_release(container_id, released, cx);
    }

    /// Tear a released session worktree down after [`CLOSE_GRACE`]. It stays
    /// on the project's record until the answer is in: a quit in between
    /// leaves it owned, and its ports stay taken while a server may still
    /// hold them.
    fn spawn_session_worktree_release(
        &mut self,
        container_id: u64,
        released: ManagedWorktree,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(CLOSE_GRACE).await;
                // Read after the grace, not before it: a project opened on
                // the worktree in the meantime must keep it.
                let Some(open) =
                    cx.update(|cx| this.update(cx, |app, _| app.every_container_path()).ok())
                else {
                    return;
                };
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

    /// Every open project's directory, none excluded.
    fn every_container_path(&self) -> Vec<std::path::PathBuf> {
        self.workspaces
            .iter()
            .map(|ws| std::path::PathBuf::from(&ws.cwd))
            .collect()
    }

    fn session_worktree_released(
        &mut self,
        container_id: u64,
        released: ManagedWorktree,
        outcome: Teardown,
        cx: &mut Context<Self>,
    ) {
        let branch = released.branch.clone();
        let removed = matches!(outcome, Teardown::Removed { .. });
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
                // Still the project's, so its close tries again.
                self.show_toast(
                    format!(
                        "Worktree for {branch} kept: {reason} - {}",
                        released.path.display()
                    ),
                    cx,
                );
            }
        }
        if removed && let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == container_id) {
            ws.managed_worktrees.retain(|wt| wt.path != released.path);
            self.save_session(cx);
        }
    }

    /// The worktree made for the session `thread_id`, when that session is
    /// the last one working in it - the one whose close decides its fate.
    fn last_session_worktree(&self, thread_id: u64) -> Option<(u64, ManagedWorktree)> {
        let ws = self
            .workspaces
            .iter()
            .find(|ws| ws.threads.iter().any(|t| t.id == thread_id))?;
        let thread = ws.threads.iter().find(|t| t.id == thread_id)?;
        let cwd = std::path::Path::new(&thread.cwd);
        let wt = ws
            .managed_worktrees
            .iter()
            .find(|wt| wt.base.is_some() && cwd.starts_with(&wt.path))?;
        let others = ws
            .threads
            .iter()
            .filter(|t| t.id != thread_id)
            .any(|t| std::path::Path::new(&t.cwd).starts_with(&wt.path));
        (!others).then(|| (ws.id, wt.clone()))
    }

    /// Before the delete card for `thread_id` opens, read what its branch
    /// holds, so the card can say. Returns whether it did: the card then
    /// opens when the read is in.
    pub(crate) fn ask_about_session_branch(
        &mut self,
        thread_id: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((_, wt)) = self.last_session_worktree(thread_id) else {
            return false;
        };
        let examined = wt.clone();
        self.agents_view.delete_branch_asking = Some(thread_id);
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let work = smol::unblock(move || session_branch::branch_work(&examined)).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        if app.agents_view.delete_branch_asking != Some(thread_id) {
                            return;
                        }
                        app.agents_view.delete_branch_asking = None;
                        app.agents_view.delete_branch = Some(DeleteBranch {
                            thread_id,
                            worktree: wt,
                            work: work.ok(),
                        });
                        app.agents_view.agents_confirm_delete =
                            Some(crate::app::agents_sidebar::AgentsDeleteTarget::Thread {
                                thread_id,
                            });
                        cx.notify();
                    })
                });
            },
        )
        .detach();
        true
    }

    /// The card's first answer: the session goes, its worktree and branch
    /// stay. The worktree leaves the project's record, so no close tears it
    /// down; the palette's Branches tab is where it is found again.
    pub(crate) fn delete_session_keep_branch(&mut self, cx: &mut Context<Self>) {
        let Some(asked) = self.agents_view.delete_branch.take() else {
            return;
        };
        self.agents_view.agents_confirm_delete = None;
        let title = self.forget_session_worktree(&asked);
        let wt = asked.worktree;
        let branch = wt.branch.clone();
        cx.background_executor()
            .spawn(async move { session_branch::describe_branch(&wt, &title) })
            .detach();
        self.remove_session_by_id(asked.thread_id, cx);
        self.push_toast(
            format!("Branch {branch} kept"),
            vec![crate::app::notifications::ToastAction::ShowBranches],
            crate::app::constants::TOAST_HOLD_MS * 2,
            cx,
        );
    }

    /// The card's second answer: the session, its worktree and its branch
    /// all go, after a snapshot of whatever was in them.
    pub(crate) fn delete_session_and_branch(&mut self, cx: &mut Context<Self>) {
        let Some(asked) = self.agents_view.delete_branch.take() else {
            return;
        };
        self.agents_view.agents_confirm_delete = None;
        // Another session may have come to work in the worktree since the
        // card opened. Then only this session goes.
        if self
            .last_session_worktree(asked.thread_id)
            .is_none_or(|(_, wt)| wt.path != asked.worktree.path)
        {
            self.remove_session_by_id(asked.thread_id, cx);
            self.show_toast(
                format!(
                    "{} kept: another session works in it",
                    asked.worktree.branch
                ),
                cx,
            );
            return;
        }
        let title = self.forget_session_worktree(&asked);
        let container_id = self
            .workspaces
            .iter()
            .find(|ws| ws.threads.iter().any(|t| t.id == asked.thread_id))
            .map(|ws| ws.id);
        self.remove_session_by_id(asked.thread_id, cx);
        let wt = asked.worktree;
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(CLOSE_GRACE).await;
                let discarding = wt.clone();
                let result =
                    smol::unblock(move || session_branch::discard(&discarding, &title)).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| match result {
                        Ok(Some(_)) => app.push_toast(
                            format!(
                                "Deleted {} - a snapshot is kept for {} days",
                                wt.branch,
                                session_branch::SNAPSHOT_DAYS
                            ),
                            vec![crate::app::notifications::ToastAction::ShowBranches],
                            crate::app::constants::TOAST_HOLD_MS * 2,
                            cx,
                        ),
                        Ok(None) => {}
                        Err(error) => {
                            app.show_toast(format!("{} kept: {error}", wt.branch), cx);
                            // Not deleted, so still the project's - if it is
                            // still there to own.
                            if wt.path.exists()
                                && let Some(ws) = container_id
                                    .and_then(|id| app.workspaces.iter_mut().find(|ws| ws.id == id))
                            {
                                ws.managed_worktrees.push(wt);
                                app.save_session(cx);
                            }
                        }
                    })
                });
            },
        )
        .detach();
    }

    /// Take the asked-about worktree off its project's record, so closing the
    /// session does not also start the quiet teardown. Returns the session's
    /// name, for the branch's description or the snapshot.
    fn forget_session_worktree(&mut self, asked: &DeleteBranch) -> String {
        let mut title = String::new();
        for ws in &mut self.workspaces {
            if let Some(thread) = ws.threads.iter().find(|t| t.id == asked.thread_id) {
                title = crate::project::clean_sidebar_title(&thread.title)
                    .unwrap_or_else(|| thread.title.clone());
                ws.managed_worktrees
                    .retain(|wt| wt.path != asked.worktree.path);
            }
        }
        title
    }

    fn remove_session_by_id(&mut self, thread_id: u64, cx: &mut Context<Self>) {
        if let Some(crate::project::AgentsTarget::Thread { ws_idx, thread_idx }) =
            crate::project::find_surface(&self.workspaces, thread_id)
        {
            let _ = self.remove_thread(ws_idx, thread_idx, cx);
        }
        self.save_session(cx);
        cx.notify();
    }
}

/// What a session's delete card says about its branch.
#[derive(Debug, Clone)]
pub(crate) struct DeleteBranch {
    pub(crate) thread_id: u64,
    pub(crate) worktree: ManagedWorktree,
    /// `None` when git could not be read: the card then asks without numbers.
    pub(crate) work: Option<BranchWork>,
}

impl DeleteBranch {
    /// Whether the card has to offer a choice: something is in the branch, or
    /// what is in it could not be read.
    pub(crate) fn has_work(&self) -> bool {
        self.work.is_none_or(|work| !work.is_empty())
    }

    /// The sentence after the branch's name.
    pub(crate) fn what_it_holds(&self) -> String {
        let Some(work) = self.work else {
            return "could not be read, so it may hold work.".to_string();
        };
        let files = match work.changed_files {
            0 => None,
            1 => Some(format!(
                "1 changed file (+{} \u{2212}{})",
                work.insertions, work.deletions
            )),
            n => Some(format!(
                "{n} changed files (+{} \u{2212}{})",
                work.insertions, work.deletions
            )),
        };
        let commits = match work.commits {
            0 => None,
            1 => Some("1 commit that is on no other branch".to_string()),
            n => Some(format!("{n} commits that are on no other branch")),
        };
        match (files, commits) {
            (Some(files), Some(commits)) => format!("has {files} and {commits}."),
            (Some(one), None) | (None, Some(one)) => format!("has {one}."),
            (None, None) => "has nothing new and goes too.".to_string(),
        }
    }
}

impl SplitlaneApp {
    /// A session row's hover text about its branch: the branch and where it
    /// came from, or whose worktree the session shares, and its port. Or why
    /// it has no branch.
    /// The managed worktree `thread` works in, if it works in one.
    pub(crate) fn session_worktree_of(
        &self,
        ws_idx: usize,
        thread: &crate::project::Thread,
    ) -> Option<&crate::workspace::worktree::ManagedWorktree> {
        let cwd = std::path::Path::new(&thread.cwd);
        self.workspaces
            .get(ws_idx)?
            .managed_worktrees
            .iter()
            .find(|wt| cwd.starts_with(&wt.path))
    }

    pub(crate) fn session_branch_tooltip(
        &self,
        ws_idx: usize,
        thread: &crate::project::Thread,
    ) -> Option<String> {
        if let Some(reason) = &thread.worktree_failed {
            return Some(format!(
                "{}\nNo branch: {reason}\nOpen without branch from the row's menu",
                thread.title
            ));
        }
        let ws = self.workspaces.get(ws_idx)?;
        let wt = self.session_worktree_of(ws_idx, thread)?;
        let opener = thread
            .opened_by
            .as_ref()
            .and_then(|by| ws.threads.iter().find(|t| t.id == by.id))
            .filter(|opener| std::path::Path::new(&opener.cwd).starts_with(&wt.path));
        let mut lines = vec![thread.title.clone()];
        lines.push(match (opener, &wt.from) {
            (Some(opener), _) => format!("{} \u{b7} in {}'s worktree", wt.branch, opener.title),
            (None, Some(from)) => format!("{} \u{b7} from {from}", wt.branch),
            (None, None) => wt.branch.clone(),
        });
        if let Some(port) = wt.port {
            lines.push(format!("Port {port}"));
        }
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(work: Option<BranchWork>) -> DeleteBranch {
        DeleteBranch {
            thread_id: 1,
            worktree: ManagedWorktree {
                path: std::path::PathBuf::from("/work/app.worktrees/splitlane-ab12cd"),
                repo_root: std::path::PathBuf::from("/work/app"),
                branch: "splitlane/ab12cd".to_string(),
                teardown: worktree::TeardownPolicy::Auto,
                base: None,
                port: None,
                from: None,
            },
            work,
        }
    }

    /// The three sentences the design gives, naming only what is there.
    #[test]
    fn the_card_names_only_what_the_branch_holds() {
        let both = BranchWork {
            changed_files: 3,
            insertions: 42,
            deletions: 7,
            commits: 2,
        };
        assert_eq!(
            asked(Some(both)).what_it_holds(),
            "has 3 changed files (+42 \u{2212}7) and 2 commits that are on no other branch."
        );
        let commits = BranchWork {
            commits: 2,
            ..BranchWork::default()
        };
        assert_eq!(
            asked(Some(commits)).what_it_holds(),
            "has 2 commits that are on no other branch."
        );
        let changes = BranchWork { commits: 0, ..both };
        assert_eq!(
            asked(Some(changes)).what_it_holds(),
            "has 3 changed files (+42 \u{2212}7)."
        );
        assert!(!asked(Some(BranchWork::default())).has_work());
        assert_eq!(
            asked(Some(BranchWork::default())).what_it_holds(),
            "has nothing new and goes too."
        );
        // Unreadable is treated as work: the card asks rather than deletes.
        assert!(asked(None).has_work());
    }
}
