//! Affordance handlers for the Agents-mode sidebar: create / rename /
//! delete / duplicate, plus the cross-cutting reveal-in-file-manager and
//! open-in-editor entry points.
//!
//! Every handler mutates `SplitlaneApp` state in-place, calls
//! `cx.notify()` and `save_session(cx)` so the change is persisted
//! across restarts, and -- when a deletion involves a row that has
//! been written to `threads.db` -- cascades the SQL DELETE so the
//! durable store does not accumulate orphan rows.

use gpui::{AppContext, Context, PathPromptOptions, Pixels, Point, Window};

use super::state::{AgentsContextMenu, AgentsDeleteTarget, AgentsRenameTarget};
use crate::SplitlaneApp;
use crate::app::workspace_ops::reveal_in_file_manager;
use crate::widgets::text_area::TextArea;

impl SplitlaneApp {
    // ------------------------------------------------------------------
    // Open / close context menus + confirmation dialog
    // ------------------------------------------------------------------

    /// Open the project-row context menu at the click position.
    /// Closes any prior menu and cancels any pending rename so the
    /// menu opens on a clean slate.
    /// Open a container's context menu. One menu per object: the rail's own
    /// project menu and the CLI rail's workspace menu described the same
    /// container and merged into `render_workspace_context_menu`.
    pub(crate) fn open_agents_project_menu(
        &mut self,
        ws_idx: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if ws_idx >= self.workspaces.len() {
            return;
        }
        self.cancel_agents_rename(cx);
        self.agents_view.agents_menu_open = None;
        self.workspace_menu_open = Some(crate::WorkspaceContextMenu {
            idx: ws_idx,
            position,
        });
        cx.notify();
    }

    pub(crate) fn open_agents_thread_menu(
        &mut self,
        ws_idx: usize,
        thread_idx: usize,
        position: Point<Pixels>,
        origin: super::MenuOrigin,
        cx: &mut Context<Self>,
    ) {
        self.cancel_agents_rename(cx);
        self.dismiss_transient_surfaces();
        self.agents_view.agents_menu_open = Some(AgentsContextMenu::Thread {
            ws_idx,
            thread_idx,
            position,
            origin,
        });
        cx.notify();
    }

    pub(crate) fn close_agents_menu(&mut self, cx: &mut Context<Self>) {
        if self.agents_view.agents_menu_open.take().is_some() {
            cx.notify();
        }
    }

    /// Queue a confirmation dialog for `target`. Idempotent: replacing
    /// a pending confirm with a fresh one is fine.
    pub(crate) fn request_agents_confirm_delete(
        &mut self,
        target: AgentsDeleteTarget,
        cx: &mut Context<Self>,
    ) {
        self.agents_view.agents_menu_open = None;
        self.agents_view.agents_confirm_delete = Some(target);
        cx.notify();
    }

    pub(crate) fn cancel_agents_confirm_delete(&mut self, cx: &mut Context<Self>) {
        if self.agents_view.agents_confirm_delete.take().is_some() {
            cx.notify();
        }
    }

    // ------------------------------------------------------------------
    // Rename machinery
    // ------------------------------------------------------------------

    /// Enter inline-rename mode for `target`. Creates a fresh
    /// [`TextArea`] entity (real editable input -- cursor / selection
    /// / IME / clipboard, mirrors what the chat composer uses) and
    /// focuses it so the next keystroke lands in the field.
    ///
    /// Cancels any prior rename before starting -- only one row can
    /// be renaming at a time.
    pub(crate) fn begin_agents_rename(
        &mut self,
        target: AgentsRenameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_agents_rename(cx);
        let current = match target {
            AgentsRenameTarget::Project { ws_idx } => self
                .workspaces
                .get(ws_idx)
                .map(|p| p.title.clone())
                .unwrap_or_default(),
            AgentsRenameTarget::Thread { ws_idx, thread_idx } => self
                .workspaces
                .get(ws_idx)
                .and_then(|p| p.threads.get(thread_idx))
                .map(|t| t.title.clone())
                .unwrap_or_default(),
        };
        let app_weak = cx.weak_entity();
        let textarea = cx.new(|cx| {
            let mut ta = TextArea::new("New name", cx);
            ta.set_value(current, cx);
            // Pre-select the whole text so the user can just start typing
            // to replace the existing name - saves a manual Ctrl+A and
            // matches the inline-rename UX in mainstream editors.
            ta.select_all_text(cx);
            // on_submit fires from INSIDE the TextArea's update, so
            // any re-read of the entity (`ta.read(cx)`) from the
            // callback panics with "cannot read while it is already
            // being updated". Pass the text the callback already
            // hands us straight through to `apply_agents_rename`,
            // which never touches the entity.
            let weak_submit = app_weak.clone();
            ta.on_submit(move |text, _w, app| {
                let _ = weak_submit
                    .clone()
                    .update(app, |this, cx| this.apply_agents_rename(text, cx));
            });
            let weak_escape = app_weak;
            ta.on_escape(move |_w, app| {
                let _ = weak_escape
                    .clone()
                    .update(app, |this, cx| this.cancel_agents_rename(cx));
            });
            ta
        });
        let focus = textarea.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        self.agents_view.agents_renaming = Some(target);
        self.agents_view.agents_rename_input = Some(textarea);
        // `agents_rename_text` is kept only as a legacy bridge for
        // call sites that haven't been migrated to read from the
        // TextArea entity yet; left empty on purpose.
        self.agents_view.agents_rename_text.clear();
        cx.notify();
    }

    /// Apply the in-progress rename, reading the latest value from
    /// the TextArea entity itself. Called from non-callback paths
    /// (e.g. click-outside, context-menu open) where the entity is
    /// NOT currently in an `update()` call and can safely be read.
    ///
    /// Code coming from the TextArea's `on_submit` callback must use
    /// [`Self::apply_agents_rename`] instead (it receives the text
    /// via the callback parameter and avoids re-entering the entity).
    pub(crate) fn commit_agents_rename(&mut self, cx: &mut Context<Self>) {
        if self.agents_view.agents_renaming.is_none() {
            return;
        }
        // Re-entrancy guardrail: `commit_agents_rename` reads the
        // TextArea entity, which panics if called from inside the
        // TextArea's own `on_submit` / `on_change` callback. Callers
        // coming from a TextArea callback must route through
        // `apply_agents_rename(text, cx)` (it accepts the value as a
        // parameter and never touches the entity). Document the
        // contract in debug builds so a future caller misuse trips
        // here loudly rather than corrupting GPUI's RefCell state.
        debug_assert!(
            self.agents_view.agents_rename_input.is_some(),
            "commit_agents_rename invariant: rename input must exist when renaming is active",
        );
        let text = self
            .agents_view
            .agents_rename_input
            .as_ref()
            .map(|ta| ta.read(cx).value())
            .unwrap_or_default();
        self.apply_agents_rename(text, cx);
    }

    /// Apply `text` as the new title for whatever row is currently
    /// in rename mode, then exit rename mode. Empty / whitespace-only
    /// text is treated as "user gave up" and rolls back without
    /// touching the title.
    ///
    /// Safe to call from inside the TextArea entity's `update` (the
    /// `on_submit` callback path): the text is taken as a parameter
    /// instead of read from the entity, and the entity is dropped on
    /// the next event-loop tick via `cx.defer` so we never re-enter
    /// the in-flight update.
    pub(crate) fn apply_agents_rename(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(target) = self.agents_view.agents_renaming.take() else {
            return;
        };
        // Drop the TextArea entity on the next tick to avoid any
        // re-entrancy when this is invoked from on_submit (where we
        // are still inside the entity's update).
        let weak = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = weak.update(cx, |app, cx| {
                app.agents_view.agents_rename_input = None;
                app.agents_view.agents_rename_text.clear();
                cx.notify();
            });
        });
        let text = text.trim().to_string();
        if text.is_empty() {
            cx.notify();
            return;
        }
        match target {
            AgentsRenameTarget::Project { ws_idx } => {
                if let Some(project) = self.workspaces.get_mut(ws_idx) {
                    project.title = text;
                    self.save_session(cx);
                }
            }
            AgentsRenameTarget::Thread { ws_idx, thread_idx } => {
                let mut renamed = None;
                if let Some(thread) = self
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|p| p.threads.get_mut(thread_idx))
                {
                    thread.title = text.clone();
                    // Lock the label: OSC updates and the ai-title backfill
                    // must not clobber a deliberate name.
                    thread.title_user_set = true;
                    renamed = Some(thread.id);
                }
                // The slot header names the surface from the terminal's own
                // `custom_name`, not from the record, so a rename here showed
                // in the rail and nowhere else. The other direction writes
                // both too; one gesture, one name, wherever it is read.
                if let Some(thread_id) = renamed
                    && let Some(view) = self
                        .agents_view
                        .agents_terminal_view_cache
                        .get(&thread_id)
                        .cloned()
                {
                    view.update(cx, |view, _| view.terminal.custom_name = Some(text));
                }
                self.save_session(cx);
            }
        }
        cx.notify();
    }

    /// Drop the in-progress rename without applying. Used when the
    /// user presses Escape, opens a context menu, or clicks elsewhere.
    /// Defers the entity drop to the next tick so a call from the
    /// TextArea's `on_escape` callback never re-enters the in-flight
    /// entity update.
    pub(crate) fn cancel_agents_rename(&mut self, cx: &mut Context<Self>) {
        if self.agents_view.agents_renaming.take().is_some() {
            let weak = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |app, cx| {
                    app.agents_view.agents_rename_input = None;
                    app.agents_view.agents_rename_text.clear();
                    cx.notify();
                });
            });
        }
    }

    // ------------------------------------------------------------------
    // Create / Duplicate
    // ------------------------------------------------------------------

    /// Create a new project by prompting the user for one or more
    /// directories via the OS folder picker. Mirrors the CLI sidebar's
    /// `create_workspace_with_picker`: each chosen folder becomes a
    /// fresh project, with the folder's basename as the title. The
    /// most recently created project is selected so the threads list
    /// opens onto it.
    pub(crate) fn create_agents_project_with_picker(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                if let Ok(Ok(Some(paths))) = receiver.await {
                    let _ = cx.update(|cx| {
                        this.update(cx, |app, cx| {
                            for path in paths {
                                let title = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "New project".to_string());
                                // `create_container` selects what it made, so
                                // the rail opens onto the last folder picked.
                                if app.create_container(title, path, cx).is_none() {
                                    app.show_toast("Too many projects open", cx);
                                }
                            }
                            // And it stops there. This used to open the
                            // launcher on top of the shell `create_container`
                            // had just spawned, so adding a project showed two
                            // panes straight away - reported from real use -
                            // one of them a picker nobody had asked for. Both
                            // are gone: the rail row's `+ agent` / `+ shell` /
                            // `+ worktree` are the doors in, and the empty
                            // area says so.
                            app.save_session(cx);
                            cx.notify();
                        })
                    });
                }
            },
        )
        .detach();
    }

    /// "New agent" with no agent named yet: open the launcher in a pane of
    /// `ws_idx`. No session is created until the user picks one.
    ///
    /// The launcher used to take the whole work area, which `FOCUS.md` rules
    /// out - "The launcher is not a screen and never takes the whole work
    /// area" - so it goes through the ladder like anything else that opens.
    /// Where the ladder makes a new pane, that pane is created empty and the
    /// launcher IS its content; where it lands on a pane that already holds
    /// something, the launcher covers it for as long as the choice is open and
    /// closes nothing.
    pub(crate) fn create_agents_thread_in(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        if ws_idx >= self.workspaces.len() {
            return;
        }
        self.active_idx = ws_idx;
        let pane = match self.target_pane_for(ws_idx, crate::app::targeting::SurfaceKind::Agent, cx)
        {
            Some(crate::app::targeting::PaneTarget::Existing(pane)) => {
                pane.update(cx, |pane, cx| pane.set_launching(true, cx));
                Some(pane)
            }
            Some(crate::app::targeting::PaneTarget::NewPane) => self.append_empty_pane(ws_idx, cx),
            None => None,
        };
        let Some(pane) = pane else {
            return;
        };
        self.pending_pane_focus = Some(pane);
        cx.notify();
    }

    /// Which container owns `pane`.
    pub(crate) fn workspace_index_of_pane(
        &self,
        pane: &gpui::Entity<crate::Pane>,
        _cx: &Context<Self>,
    ) -> Option<usize> {
        self.workspaces.iter().position(|container| {
            container
                .root
                .as_ref()
                .is_some_and(|root| root.contains_leaf(pane))
        })
    }

    /// Picker selection: create a Terminal Thread bound to `agent` in
    /// `ws_idx` and select it. The agent CLI is auto-launched on
    /// the explicit PTY mount in
    /// [`SplitlaneApp::mount_agents_terminal_for_target`] (which reads the
    /// thread's `terminal_agent` and honors the bypass-permission flag).
    pub(crate) fn create_agent_terminal_thread_in(
        &mut self,
        ws_idx: usize,
        agent: crate::agent_launcher::TerminalAgent,
        cx: &mut Context<Self>,
    ) {
        if !agent.is_installed() {
            self.show_toast(format!("{} is not installed", agent.display_name()), cx);
            return;
        }
        let new_thread_id =
            match self.add_terminal_thread(ws_idx, agent.display_name(), Some(agent), cx) {
                Ok(id) => id,
                Err(err) => {
                    self.show_toast(format!("Could not create thread: {err:?}"), cx);
                    return;
                }
            };
        if let Some(project) = self.workspaces.get(ws_idx)
            && let Some(thread_idx) = project.threads.iter().position(|t| t.id == new_thread_id)
        {
            let _ = self.select_thread(ws_idx, thread_idx, cx);
        }
        self.save_session(cx);
    }

    /// Create an agent session whose working directory is `cwd` rather than
    /// the project's own - what `+ worktree` starts in the tree it just made.
    ///
    /// The session stays a session **of this project**: the rail lists it
    /// there, the diff it opens is the one under `cwd`, and closing the
    /// project takes it with it. Only the directory differs.
    pub(crate) fn create_agent_session_in_cwd(
        &mut self,
        ws_idx: usize,
        agent: crate::agent_launcher::TerminalAgent,
        title: impl Into<String>,
        cwd: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        if !agent.is_installed() {
            self.show_toast(format!("{} is not installed", agent.display_name()), cx);
            return;
        }
        let new_thread_id = match self.add_terminal_thread(ws_idx, title, Some(agent), cx) {
            Ok(id) => id,
            Err(err) => {
                self.show_toast(format!("Could not create session: {err:?}"), cx);
                return;
            }
        };
        // The thread is minted against the project's cwd; re-point it before
        // the first PTY mount reads it. Done here rather than through a
        // second constructor because the directory is the only difference.
        if let Some(project) = self.workspaces.get_mut(ws_idx)
            && let Some(thread) = project.threads.iter_mut().find(|t| t.id == new_thread_id)
        {
            thread.cwd = cwd.display().to_string();
        }
        if let Some(project) = self.workspaces.get(ws_idx)
            && let Some(thread_idx) = project.threads.iter().position(|t| t.id == new_thread_id)
        {
            let _ = self.select_thread(ws_idx, thread_idx, cx);
        }
        self.save_session(cx);
    }

    /// Open a Claude session from history as a Terminal Thread in
    /// `ws_idx` and select it - the Agents-mode counterpart of the
    /// CLI-mode sessions sidebar, which can only inject a resume command into
    /// a pane that already exists.
    ///
    /// The picked session's own summary becomes the thread title, so the row
    /// reads like it does in `claude --resume`. When the session is already
    /// open in this project the existing thread is selected instead of a
    /// second one being created: two PTYs sharing one transcript is precisely
    /// what the forced session id is there to prevent.
    pub(crate) fn open_session_as_thread_in(
        &mut self,
        ws_idx: usize,
        agent: crate::agent_launcher::TerminalAgent,
        session_id: &str,
        title: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        if !agent.is_installed() {
            self.show_toast(format!("{} is not installed", agent.display_name()), cx);
            return;
        }
        if let Some(existing_idx) = self.thread_idx_for_session(ws_idx, session_id) {
            let _ = self.select_thread(ws_idx, existing_idx, cx);
            self.show_toast("Session is already open", cx);
            return;
        }
        let new_thread_id = match self.add_terminal_thread_for_session(
            ws_idx,
            title,
            Some(agent),
            session_id,
            cx,
        ) {
            Ok(id) => id,
            Err(err) => {
                self.show_toast(format!("Could not open session: {err:?}"), cx);
                return;
            }
        };
        if let Some(project) = self.workspaces.get(ws_idx)
            && let Some(thread_idx) = project.threads.iter().position(|t| t.id == new_thread_id)
        {
            let _ = self.select_thread(ws_idx, thread_idx, cx);
        }
        self.save_session(cx);
    }

    /// Create a fresh bare Terminal Thread (no agent) in `ws_idx`:
    /// a plain shell in the project's cwd. Backs the secondary
    /// "New terminal thread" affordance for when the user wants a raw
    /// terminal rather than a launched agent.
    pub(crate) fn create_terminal_thread_in(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        // Named after the directory, which is how its own pane header will
        // name it: a shell reports no OSC title until something in it does,
        // and the header falls back to the cwd while the rail kept the literal
        // it was created with. So one surface read "Terminal" in the rail and
        // "splitlane" in its header and in the status bar, in the same frame -
        // and the OSC sync that would have reconciled them skips exactly the
        // word "Terminal", because that is also what alacritty stamps on a
        // `ResetTitle`.
        let title = self
            .workspaces
            .get(ws_idx)
            .and_then(|ws| crate::pane::Pane::cwd_label(&ws.cwd))
            .unwrap_or_else(|| "Terminal".to_string());
        let new_thread_id = match self.add_terminal_thread(ws_idx, title, None, cx) {
            Ok(id) => id,
            Err(err) => {
                self.show_toast(format!("Could not create terminal thread: {err:?}"), cx);
                return;
            }
        };
        if let Some(project) = self.workspaces.get(ws_idx)
            && let Some(thread_idx) = project.threads.iter().position(|t| t.id == new_thread_id)
        {
            let _ = self.select_thread(ws_idx, thread_idx, cx);
        }
        self.save_session(cx);
    }

    /// Duplicate a thread: same agent + cwd, fresh ID. The copy is a
    /// Terminal Thread bound to the source's `terminal_agent` (so it
    /// relaunches the same CLI on first open).
    pub(crate) fn duplicate_agents_thread(
        &mut self,
        ws_idx: usize,
        thread_idx: usize,
        cx: &mut Context<Self>,
    ) {
        let (terminal_agent, base_title) = match self
            .workspaces
            .get(ws_idx)
            .and_then(|p| p.threads.get(thread_idx))
        {
            Some(t) => (t.terminal_agent, t.title.clone()),
            None => return,
        };

        let new_title = format!("{base_title} (copy)");
        if let Err(err) = self.add_terminal_thread(ws_idx, new_title, terminal_agent, cx) {
            self.show_toast(format!("Could not duplicate thread: {err:?}"), cx);
            return;
        }
        self.save_session(cx);
    }

    // ------------------------------------------------------------------
    // Target-parameterized dispatch
    //
    // The Pinned and Chats sections render rows that may back either a
    // project thread OR a free chat. These helpers map a unified
    // [`crate::project::AgentsTarget`] to the right concrete handler so the
    // row widgets stay source-agnostic (no duplicated project/chat render
    // logic, which would drift apart).
    // ------------------------------------------------------------------

    /// Select whatever the target points at (a Pinned row routes to its
    /// original source).
    pub(crate) fn select_agents_target(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) {
        use crate::project::AgentsTarget;
        let _ = match target {
            AgentsTarget::Thread { ws_idx, thread_idx } => {
                self.select_thread(ws_idx, thread_idx, cx)
            }
            // `Changes` opens by the ladder like everything else - and
            // opening it when it is already in a pane just focuses that pane.
            AgentsTarget::Diff { ws_idx } => {
                self.open_container_diff_surface(ws_idx, cx);
                Ok(())
            }
            AgentsTarget::Panes { ws_idx } => {
                self.select_container_panes(ws_idx, cx);
                Ok(())
            }
        };
    }

    /// Open the container's diff as a surface: give it an id if it does not
    /// have one yet, then select it. Opening an already-open diff just
    /// selects it, like clicking its row.
    pub(crate) fn open_container_diff_surface(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        let Some(container) = self.workspaces.get(ws_idx) else {
            return;
        };
        if !container.is_git_repo {
            self.show_toast("No Git repository in this folder", cx);
            return;
        }
        let Some(repo_root) = container.repo_root.clone() else {
            self.show_toast("No Git repository in this folder", cx);
            return;
        };
        let worktrees = vec![crate::diff::DiffWorktree {
            path: container.worktree_root.clone(),
            branch: container.git_branch.clone(),
            workspace_id: Some(container.id),
        }];
        if let Some(container) = self.workspaces.get_mut(ws_idx)
            && container.diff_surface.is_none()
        {
            container.diff_surface = Some(crate::workspace::next_id());
        }
        // Already showing: its pane takes focus and nothing is rebuilt. The
        // diff is expensive to recompute and the user asked to look at it, not
        // to refresh it.
        if self.reveal_tab_in_panes(
            ws_idx,
            |tab, _| matches!(tab, crate::pane::TabContent::Diff(_)),
            cx,
        ) {
            self.rebuild_diff_view(cx);
            self.save_session(cx);
            return;
        }
        let diff = cx.new(|cx| crate::diff::DiffView::new(repo_root, worktrees, cx));
        self.place_surface(
            ws_idx,
            crate::app::targeting::SurfaceKind::Diff,
            crate::pane::TabContent::Diff(diff),
            cx,
        );
        // The file-list column is drawn from the app-side diff host, not from
        // the entity in the pane: it is built from the scope and the
        // branch/revision filter, which belong to the app. Without this the
        // column comes up reading "No Git repository" beside a diff of a repo
        // that is right there.
        self.rebuild_diff_view(cx);
    }

    /// Close the container's diff surface. Nothing is lost - the diff is
    /// recomputed from git the next time it is opened - so unlike deleting an
    /// agent surface this asks nothing.
    pub(crate) fn close_container_diff_surface(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        let Some(container) = self.workspaces.get_mut(ws_idx) else {
            return;
        };
        if container.diff_surface.take().is_none() {
            return;
        }
        // The row and the pane's content are two faces of one surface, so
        // closing the row closes the tab. Leaving the tab behind would give
        // the container a diff nothing in the rail names - the one state a
        // surface cannot be reached or closed from.
        self.close_diff_tabs(ws_idx, cx);
        self.save_session(cx);
        cx.notify();
    }

    /// Drop every diff tab of `ws_idx` from its panes.
    fn close_diff_tabs(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        let Some(root) = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
        else {
            return;
        };
        for pane in root.collect_leaves() {
            // Backwards: closing shifts every later index down.
            let indices: Vec<usize> = pane
                .read(cx)
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| matches!(tab, crate::pane::TabContent::Diff(_)))
                .map(|(idx, _)| idx)
                .rev()
                .collect();
            for idx in indices {
                pane.update(cx, |pane, cx| pane.close_tab_at(idx, cx));
            }
        }
    }

    /// Open a container's `+ agent` menu: which agent to start in it, and
    /// whether to remember that choice.
    pub(crate) fn open_new_agent_menu(
        &mut self,
        ws_idx: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.cancel_agents_rename(cx);
        self.agents_view.agents_menu_open = Some(AgentsContextMenu::NewAgent { ws_idx, position });
        cx.notify();
    }

    /// Show a container's panes: make it active and hand the keyboard to one.
    ///
    /// It used to take an `AgentsTarget` and point the content area at it -
    /// its tree, or its full-area diff. A container has one thing to show now,
    /// so the parameter went with the branch.
    pub(crate) fn select_container_panes(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        if ws_idx >= self.workspaces.len() {
            return;
        }
        self.active_idx = ws_idx;
        self.park_displayed_diff(cx);
        // Hand the keyboard to what was selected. Without this the rail row
        // keeps focus and every global chord - the palette included - goes
        // nowhere until the user clicks into a pane. The focus itself is
        // deferred to the next render, which is where the `Window` lives (the
        // same channel `DropSplit` uses).
        self.pending_pane_focus = self
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.root.as_ref())
            .and_then(|root| root.first_leaf());
    }

    /// Open the surface's menu, at `position`, for whoever asked.
    ///
    /// `origin` decides only whether the conversation entries are drawn - see
    /// [`super::MenuOrigin`]. Everything else is the same menu either way,
    /// because it is the same surface.
    pub(crate) fn open_agents_menu_for_target(
        &mut self,
        target: crate::project::AgentsTarget,
        position: Point<Pixels>,
        origin: super::MenuOrigin,
        cx: &mut Context<Self>,
    ) {
        use crate::project::AgentsTarget;
        // Opening a context menu cancels any armed inline delete-confirm.
        match target {
            AgentsTarget::Thread { ws_idx, thread_idx } => {
                self.open_agents_thread_menu(ws_idx, thread_idx, position, origin, cx)
            }
            // A container surface's menu is the container's own.
            AgentsTarget::Panes { ws_idx } | AgentsTarget::Diff { ws_idx } => {
                self.open_agents_project_menu(ws_idx, position, cx)
            }
        }
    }

    /// Begin inline rename for the target's row.
    pub(crate) fn begin_agents_rename_for_target(
        &mut self,
        target: crate::project::AgentsTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::project::AgentsTarget;
        let rename_target = match target {
            AgentsTarget::Thread { ws_idx, thread_idx } => {
                AgentsRenameTarget::Thread { ws_idx, thread_idx }
            }
            // Renaming a container surface renames the container.
            AgentsTarget::Panes { ws_idx } | AgentsTarget::Diff { ws_idx } => {
                AgentsRenameTarget::Project { ws_idx }
            }
        };
        self.begin_agents_rename(rename_target, window, cx);
    }

    /// Queue the delete confirmation for the target's row.
    pub(crate) fn request_delete_for_target(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) {
        use crate::project::AgentsTarget;
        let delete_target = match target {
            AgentsTarget::Thread { ws_idx, thread_idx } => AgentsDeleteTarget::Thread {
                thread_id: match self
                    .workspaces
                    .get(ws_idx)
                    .and_then(|container| container.threads.get(thread_idx))
                {
                    Some(thread) => thread.id,
                    None => return,
                },
            },
            // Closing the diff surface loses nothing - it is recomputed from
            // git on the next open - so it closes instead of asking.
            AgentsTarget::Diff { ws_idx } => {
                self.close_container_diff_surface(ws_idx, cx);
                return;
            }
            AgentsTarget::Panes { ws_idx } => AgentsDeleteTarget::Project { ws_idx },
        };
        self.request_agents_confirm_delete(delete_target, cx);
    }

    /// Duplicate the target's row.
    pub(crate) fn duplicate_agents_target(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) {
        use crate::project::AgentsTarget;
        match target {
            AgentsTarget::Thread { ws_idx, thread_idx } => {
                self.duplicate_agents_thread(ws_idx, thread_idx, cx)
            }
            // A container's slot tree and diff have nothing to duplicate.
            AgentsTarget::Panes { .. } | AgentsTarget::Diff { .. } => {}
        }
    }

    /// Restart the target's mounted PTY so the next render spawns it with the
    /// current terminal settings, including `default_shell`.
    pub(crate) fn restart_agents_target_terminal(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(thread_id) = self.thread_for_target(target).map(|thread| thread.id) else {
            return;
        };
        let mounted = self
            .agents_view
            .agents_terminal_view_cache
            .contains_key(&thread_id);
        self.agents_view.agents_menu_open = None;
        self.remove_agents_terminal_cache_entry(thread_id);
        if mounted {
            self.show_toast("Terminal restarting with current shell", cx);
        } else {
            self.show_toast("Terminal will start with current shell", cx);
        }
        cx.notify();
    }

    /// Toggle the pin flag on the target's thread/chat and persist.
    /// Idempotent per-click (a re-pin just flips back). Pinned threads are
    /// aggregated into the rail's PINNED section across both sources.
    pub(crate) fn toggle_pin_for_target(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) {
        use crate::project::AgentsTarget;
        let pinned = match target {
            AgentsTarget::Thread { ws_idx, thread_idx } => self
                .workspaces
                .get_mut(ws_idx)
                .and_then(|p| p.threads.get_mut(thread_idx))
                .map(|t| {
                    t.pinned = !t.pinned;
                    t.pinned
                }),
            // Only agent surfaces carry a pin.
            AgentsTarget::Panes { .. } | AgentsTarget::Diff { .. } => None,
        };
        if pinned.is_some() {
            self.save_session(cx);
            cx.notify();
        }
    }

    // ------------------------------------------------------------------
    // The home directory's container
    // ------------------------------------------------------------------

    /// The container rooted at the user's home directory, creating it if it
    /// does not exist yet. This is where free chats went: an agent surface
    /// anchored on `~` is an agent surface of the container for `~`, and the
    /// design has exactly one container type.
    ///
    /// Returns `None` only when the container list is full and no home
    /// container exists.
    pub(crate) fn home_container_idx(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        let home = crate::app::project_ops::home_container_cwd();
        if let Some(idx) = self
            .workspaces
            .iter()
            .position(|ws| splitlane_config::schema::same_directory(&ws.cwd, &home))
        {
            return Some(idx);
        }
        let title = std::path::Path::new(&home)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Home".to_string());
        let id = self.create_container(title, std::path::PathBuf::from(&home), cx)?;
        // By id, not by `len() - 1`: the caller creates an agent into whatever
        // index comes back, and "the container list only ever grows at the
        // tail" is an assumption about someone else's function.
        self.workspaces.iter().position(|ws| ws.id == id)
    }

    /// What "New chat" became: open the agent picker on the home directory's
    /// container. Nothing is created until an agent is picked - the same flow
    /// as "New thread", because it is now the same flow.
    pub(crate) fn handle_new_home_agent(
        &mut self,
        _: &crate::NewHomeAgent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_new_chat(cx);
    }

    /// Launch an agent in the active container - the keyboard and palette half
    /// of the container row's `+ agent`.
    ///
    /// A container that has already answered "which agent" gets that agent
    /// without being asked again: the `+ agent` menu marks its choice
    /// "default \u{b7} \u{2318}N", and a chord that opened the picker anyway would
    /// have been advertising something it did not do. A container with no
    /// remembered choice opens the picker, because choosing IS the decision.
    pub(crate) fn handle_new_agent(
        &mut self,
        _: &crate::NewAgent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // No container open: fall back to the home directory's, which is what
        // "new agent" means when there is nothing else to mean.
        if self.workspaces.is_empty() {
            self.start_new_chat(cx);
            return;
        }
        let ws_idx = self.active_idx;
        match self.default_agent_for(ws_idx) {
            Some(agent) => self.create_agent_terminal_thread_in(ws_idx, agent, cx),
            None => self.create_agents_thread_in(ws_idx, cx),
        }
    }

    /// The design's "New shell", and the keyboard half of the rail's
    /// `+ shell`. It opens a shell in the active container, not in whatever
    /// pane holds focus: a shell belongs to a directory. Where in that
    /// container it lands is the ladder's answer, not this one's.
    pub(crate) fn handle_new_shell(
        &mut self,
        _: &crate::NewShell,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.is_empty() {
            return;
        }
        let ws_idx = self.active_idx;
        self.create_terminal_thread_in(ws_idx, cx);
    }

    fn start_new_chat(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.home_container_idx(cx) else {
            self.show_toast("Too many projects open", cx);
            return;
        };
        self.create_agents_thread_in(idx, cx);
    }

    // ------------------------------------------------------------------
    // Execute confirmed delete
    // ------------------------------------------------------------------

    /// Apply the pending delete (container or thread). The in-memory
    /// caches are cascaded by `close_workspace_at` / `remove_thread`;
    /// Terminal Threads have no durable rows to clean up.
    pub(crate) fn execute_agents_confirm_delete(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.agents_view.agents_confirm_delete.take() else {
            return;
        };
        match target {
            AgentsDeleteTarget::Project { ws_idx } => {
                let Some(count) = self.workspaces.get(ws_idx).map(|ws| ws.threads.len()) else {
                    return;
                };
                self.close_workspace_at(ws_idx, window, cx);
                self.show_toast(
                    if count == 0 {
                        "Project deleted".to_string()
                    } else if count == 1 {
                        "Project + 1 thread deleted".to_string()
                    } else {
                        format!("Project + {count} threads deleted")
                    },
                    cx,
                );
            }
            AgentsDeleteTarget::Thread { thread_id } => {
                // Resolved here rather than when the dialog opened: the
                // indices can have moved while it stood.
                let Some(crate::project::AgentsTarget::Thread { ws_idx, thread_idx }) =
                    crate::project::find_surface(&self.workspaces, thread_id)
                else {
                    return;
                };
                if self.remove_thread(ws_idx, thread_idx, cx).is_err() {
                    return;
                }
                self.show_toast("Thread deleted", cx);
            }
        }
        self.save_session(cx);
    }

    // ------------------------------------------------------------------
    // Reveal / Open in editor (project rows)
    // ------------------------------------------------------------------

    /// Reveal the cwd of a thread/chat target in the OS file
    /// manager. A project thread reveals its own cwd (defaults to the
    /// project cwd); a chat reveals its home cwd, as the overflow menu
    /// requires. Closes any open menu first.
    pub(crate) fn reveal_agents_target(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.thread_for_target(target).map(|t| t.cwd.clone());
        self.agents_view.agents_menu_open = None;
        let Some(cwd) = cwd else {
            cx.notify();
            return;
        };
        if let Err(msg) = reveal_in_file_manager(std::path::Path::new(&cwd)) {
            log::warn!("agents-overflow: reveal failed: {msg}");
            self.show_toast(msg, cx);
        }
        cx.notify();
    }
}
