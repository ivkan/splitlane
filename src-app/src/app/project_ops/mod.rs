#![allow(dead_code)]

//! Agent-surface lifecycle operations for `SplitlaneApp`.
//!
//! Sibling of [`crate::app::workspace_ops`], which owns the container itself:
//! every method here is a thin state mutation on a container's parked agent
//! surfaces (`Workspace::threads`), followed by
//! [`cx.notify`] so the next render sees the change.
//!
//! `save_session` is invoked at the call site (matching the workspace_ops
//! pattern -- some op chains save once at the end, not after every step).

use gpui::Context;

use crate::SplitlaneApp;
use crate::project::{AgentsTarget, Thread};

/// Hard cap on parked agent surfaces per container.
pub const MAX_THREADS_PER_PROJECT: usize = 100;

/// Outcome of an op that may fail with a user-facing reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpError {
    /// Too many containers already exist.
    ProjectLimitReached,
    /// Too many threads already exist in the target container.
    ThreadLimitReached,
    /// The target container index does not exist.
    ProjectNotFound,
    /// The target thread does not exist within the target container.
    ThreadNotFound,
}

impl SplitlaneApp {
    /// Append a Terminal Thread to `ws_idx`. Same shape as
    /// [`Self::add_thread`] but stamps [`crate::project::ThreadKind::Terminal`]
    /// and never touches `threads.db` - the PTY is the source of truth
    /// for Terminal Threads and no message rows exist to persist.
    /// `terminal_agent` is the CLI auto-launched on first PTY mount
    /// (`None` for a bare shell).
    pub(crate) fn add_terminal_thread(
        &mut self,
        ws_idx: usize,
        title: impl Into<String>,
        terminal_agent: Option<crate::agent_launcher::TerminalAgent>,
        cx: &mut Context<Self>,
    ) -> Result<u64, OpError> {
        let project = self
            .workspaces
            .get_mut(ws_idx)
            .ok_or(OpError::ProjectNotFound)?;
        if project.threads.len() >= MAX_THREADS_PER_PROJECT {
            return Err(OpError::ThreadLimitReached);
        }
        let thread =
            crate::project::Thread::new_terminal(title, project.cwd.clone(), terminal_agent);
        let id = thread.id;
        project.threads.push(thread);
        cx.notify();
        Ok(id)
    }

    /// [`Self::add_terminal_thread`] bound to an existing agent session
    /// instead of a freshly minted one - the data half of "open a session
    /// from history". The thread's first PTY mount resolves the adopted id to
    /// [`crate::agent_launcher::SessionBinding::Resume`], so the conversation
    /// is restored rather than replaced.
    ///
    /// Returns [`OpError::ThreadNotFound`] when the project already holds a
    /// thread bound to this session: two live PTYs writing one transcript is
    /// exactly the collision the forced session id exists to prevent, and the
    /// caller selects the existing thread instead.
    pub(crate) fn add_terminal_thread_for_session(
        &mut self,
        ws_idx: usize,
        title: impl Into<String>,
        terminal_agent: Option<crate::agent_launcher::TerminalAgent>,
        session_id: &str,
        cx: &mut Context<Self>,
    ) -> Result<u64, OpError> {
        let project = self
            .workspaces
            .get_mut(ws_idx)
            .ok_or(OpError::ProjectNotFound)?;
        if project.threads.len() >= MAX_THREADS_PER_PROJECT {
            return Err(OpError::ThreadLimitReached);
        }
        if project
            .threads
            .iter()
            .any(|t| t.session_id.as_deref() == Some(session_id))
        {
            return Err(OpError::ThreadNotFound);
        }
        let thread = crate::project::Thread::new_terminal_for_session(
            title,
            project.cwd.clone(),
            terminal_agent,
            session_id,
        );
        let id = thread.id;
        project.threads.push(thread);
        cx.notify();
        Ok(id)
    }

    /// Index of the thread in `ws_idx` already bound to `session_id`.
    /// Lets the caller focus a session that is open rather than refusing to
    /// act on it.
    pub(crate) fn thread_idx_for_session(&self, ws_idx: usize, session_id: &str) -> Option<usize> {
        self.workspaces
            .get(ws_idx)?
            .threads
            .iter()
            .position(|t| t.session_id.as_deref() == Some(session_id))
    }

    /// Select a container's agent surface as the center target. Sets both the
    /// active container (`active_idx`) and the unified target so the rail
    /// highlight and the center stay in sync. Returns `Err` if either index is
    /// out of bounds.
    pub(crate) fn select_thread(
        &mut self,
        ws_idx: usize,
        thread_idx: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), OpError> {
        let project = self
            .workspaces
            .get(ws_idx)
            .ok_or(OpError::ProjectNotFound)?;
        if thread_idx >= project.threads.len() {
            return Err(OpError::ThreadNotFound);
        }
        let cwd = project.cwd.clone();
        let thread = &project.threads[thread_idx];
        let thread_id = thread.id;
        // An agent and a shell are different kinds to the ladder - "the left
        // half stays conversation, the right half stays terminal" is the whole
        // point of its first rung - and the rail's own test is the one that
        // decides which this is.
        let kind = if crate::app::agents_sidebar::is_shell_surface(thread) {
            crate::app::targeting::SurfaceKind::Shell
        } else {
            crate::app::targeting::SurfaceKind::Agent
        };
        self.spawn_agents_environment_git_refresh(cwd, cx);
        // Already open in a pane: nothing is replaced, its pane simply takes
        // focus. It is on screen next to whatever it was split beside, and
        // re-placing it would tear apart the composition the user built.
        if self.agent_surface_in_slot(ws_idx, thread_id, cx) {
            self.reveal_agent_in_slot(ws_idx, thread_id, cx);
            return Ok(());
        }
        self.active_idx = ws_idx;
        let target = AgentsTarget::Thread { ws_idx, thread_idx };
        let Some(view) = self.mount_agents_terminal_for_target(target, cx) else {
            return Err(OpError::ThreadNotFound);
        };
        // The ladder decides where it lands, and `place_surface` hands it the
        // keyboard through `pending_pane_focus`: opening something always
        // focuses it, and a rail click has no `Window` of its own.
        self.place_surface(ws_idx, kind, crate::pane::TabContent::Terminal(view), cx);
        Ok(())
    }

    /// Remove a thread by index within the given project. Returns
    /// the removed [`Thread`] so the caller can cascade-delete its
    /// row from `splitlane-threads`.
    pub(crate) fn remove_thread(
        &mut self,
        ws_idx: usize,
        thread_idx: usize,
        cx: &mut Context<Self>,
    ) -> Result<Thread, OpError> {
        let project = self
            .workspaces
            .get_mut(ws_idx)
            .ok_or(OpError::ProjectNotFound)?;
        if thread_idx >= project.threads.len() {
            return Err(OpError::ThreadNotFound);
        }
        let removed = project.threads.remove(thread_idx);
        // A surface showing in a slot has to leave the slot too, or deleting
        // its row would leave a running PTY in the layout with nothing in the
        // rail naming it - the one state from which an agent cannot be
        // reached, stopped, or resumed.
        self.remove_agent_from_slots(ws_idx, removed.id, cx);
        // Drop the cached Terminal Thread entity so its PTY is torn down
        // (the alacritty event loop sends `Msg::Shutdown` in `Drop`, see
        // `src/terminal/pty_session.rs`).
        self.agents_view
            .agents_terminal_view_cache
            .remove(&removed.id);
        // Keep the unified target in range. Only a target pointing
        // into THIS project's thread list is affected; a different
        // container's target is left untouched. Removing the
        // selected thread clears the target (falls back to the picker);
        // removing an earlier sibling shifts the index down.
        cx.notify();
        Ok(removed)
    }
}

/// The home directory as a container cwd. Cross-platform via
/// `dirs::home_dir()` - never `$HOME` raw, never a hardcoded POSIX path.
/// Documented fallback chain when home cannot be resolved: the current working
/// directory, then `"."` (always a valid relative cwd). Never panics.
pub(crate) fn home_container_cwd() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| ".".to_string())
}

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_container_cwd_never_empty() {
        let cwd = home_container_cwd();
        assert!(!cwd.trim().is_empty());
    }
}
