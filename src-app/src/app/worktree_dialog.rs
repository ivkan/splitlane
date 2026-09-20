//! `+ worktree`: the rail's third way to start work in a project.
//!
//! The design puts worktree creation where the project lives - a dashed
//! `+ worktree` next to `+ agent` and `+ shell`, opening a 520-wide dialog
//! that asks for a branch name, states what it is based on and where it will
//! be created, and offers one checkbox: "Start an agent session in it".
//!
//! It says *state* rather than *ask* for two of its four rows on purpose. The
//! base branch is the project's current one and the directory is derived from
//! the branch name (`<repo>.worktrees/<slug>`, or a hashed sibling when that
//! slug is already claimed) - neither is a choice this app offers, so neither
//! gets a field. The same reasoning the settings rows follow.
//!
//! The engine underneath is the one `splitlane up` and the Launch Pad already
//! use: [`worktree::add_worktree`] off the render thread, then a best-effort
//! `.env*` copy, then [`worktree::run_setup`], then a [`ManagedWorktree`]
//! registration. A failure surfaces git's error verbatim and creates nothing.
//!
//! **Two of its five rows are parity with the preset, and they are the two
//! that ask.** A preset pane's worktree carries `setup` and
//! `worktree_teardown`; a worktree cut from the rail used to carry neither as
//! a choice - it ran no bootstrap at all, and its teardown was the preset's
//! own default (`auto`) with no way to say otherwise. So `Setup` and `Remove
//! it when the project closes` are here, and the setup command is
//! **remembered on the project** ([`crate::workspace::Workspace::worktree_setup`]):
//! a bootstrap is a property of the repository, not of the branch somebody
//! just cut, so it is typed once and stated thereafter.
//!
//! The runner itself deliberately does **not** live here.
//! [`worktree::add_worktree`] has two callers - this dialog and
//! `cli/up_cmd.rs::execute_worktree_plan` - so a setup hook screwed into the
//! dialog alone would silently skip `splitlane up`.

use gpui::{
    AnyElement, ClickEvent, Context, Entity, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, Styled, Window, deferred, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;
use crate::widgets::text_input::TextInput;
use crate::workspace::worktree::{self, ManagedWorktree};

/// Live `+ worktree` dialog state, owned by `SplitlaneApp`.
pub(crate) struct WorktreeDialogState {
    /// Container the worktree belongs to, by stable id (survives reorders and
    /// closes - re-resolved when the background work returns).
    pub(crate) container_id: u64,
    /// Repository the worktree is cut from. Captured at open so the dialog
    /// can state the directory before anything runs.
    pub(crate) repo_root: std::path::PathBuf,
    /// The branch the new one is based on - the project's current branch.
    pub(crate) based_on: String,
    /// The slug directories the repository's existing worktrees already
    /// occupy, read once when the dialog opened.
    ///
    /// The planner moves a colliding branch to a hashed sibling, and the
    /// Directory row has to say the same path the confirm will create. It
    /// cannot ask git per frame, so it asks once - a worktree added from
    /// outside while this dialog is open is a race the planner still catches,
    /// and the row would rather be a frame stale than wrong for every
    /// keystroke.
    pub(crate) claimed_dirs: Vec<std::path::PathBuf>,
    pub(crate) branch_input: Entity<TextInput>,
    /// What to run in the new tree once it exists. Prefilled from the
    /// project's remembered value, and written back to it on a successful
    /// creation - the same command answers every worktree of one repository.
    pub(crate) setup_input: Entity<TextInput>,
    /// "Start an agent session in it", checked per the design.
    pub(crate) start_agent: bool,
    /// "Remove it when the project closes" - the dialog's word for
    /// [`worktree::TeardownPolicy`]. Checked, because `auto` is what this
    /// door has always registered and what a preset pane that names no
    /// policy gets.
    pub(crate) teardown_auto: bool,
    /// `true` while git runs - disables re-submission and Escape.
    pub(crate) running: bool,
    /// `true` once git is done and the person's own `setup` command is what
    /// the dialog is waiting on. Its own flag rather than a phase word,
    /// because that wait can run for the whole
    /// [`worktree::DEFAULT_SETUP_TIMEOUT`]: a button still saying
    /// "Creating..." five minutes after git finished would be naming the
    /// wrong process.
    pub(crate) running_setup: bool,
    /// Last failure, shown verbatim (git stderr included).
    pub(crate) error: Option<String>,
}

/// Where a new worktree for `branch` can go, and whether the branch itself
/// has to be created.
///
/// The slug path is preferred; a second branch whose slug collides with a
/// live worktree moves to the hashed sibling. Every other collision is an
/// error rather than a guess - this function is read-only, and it is what
/// `--dry-run` would report if the dialog had one.
pub(crate) fn plan_new_worktree(
    repo_root: &std::path::Path,
    branch: &str,
) -> Result<(std::path::PathBuf, bool), String> {
    let legacy_path = worktree::worktree_dir(repo_root, branch);
    let hashed_path = worktree::worktree_dir_hashed(repo_root, branch);
    let entries = worktree::list_worktrees(repo_root)?;
    let mut path = legacy_path.clone();
    for entry in &entries {
        if entry.branch.as_deref() == Some(branch) {
            return Err(format!(
                "branch '{branch}' is already checked out at {}",
                entry.path.display()
            ));
        }
        if entry.path == legacy_path {
            path = hashed_path.clone();
        }
    }
    for entry in &entries {
        if entry.path == path {
            return Err(format!(
                "{} exists but holds another branch ({})",
                path.display(),
                entry.branch.as_deref().unwrap_or("detached")
            ));
        }
    }
    if path.exists() {
        return Err(format!(
            "{} exists but is not a registered worktree; remove it first",
            path.display()
        ));
    }
    Ok((path, !worktree::branch_exists(repo_root, branch)))
}

/// One of the dialog's two checkboxes.
///
/// A free function rather than a closure in `render`: two rows differing only
/// in their label and which flag they flip is exactly the shape that drifts a
/// pixel apart when it is written twice, and the second one arrived with the
/// teardown choice.
fn checkbox_row(
    id: &'static str,
    label: &'static str,
    checked: bool,
    running: bool,
    toggle: fn(&mut WorktreeDialogState),
    cx: &mut Context<SplitlaneApp>,
) -> AnyElement {
    let ui = crate::theme::ui_colors();
    let box_fill = if checked { ui.accent } else { ui.subtle };
    let box_border = if checked { ui.accent } else { ui.border_strong };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::MD)
        .when(!running, |d| d.cursor_pointer())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
            if let Some(wd) = this.worktree_dialog.as_mut()
                && !wd.running
            {
                toggle(wd);
                cx.notify();
            }
            cx.stop_propagation();
        }))
        .child(
            div()
                .flex_none()
                .size(px(14.))
                .rounded(tok::radius::BADGE)
                .border_1()
                .border_color(box_border)
                .bg(box_fill)
                .flex()
                .items_center()
                .justify_center()
                .text_size(tok::mono::HINT)
                .text_color(crate::theme::text_on_fill(box_fill))
                .when(checked, |d| d.child("\u{2713}")),
        )
        .child(
            div()
                .text_size(tok::text::ROW)
                .text_color(ui.text)
                .child(label),
        )
        .into_any_element()
}

/// Everything the background-completion handler needs.
struct WorktreePlan {
    container_id: u64,
    repo_root: std::path::PathBuf,
    branch: String,
    start_agent: bool,
    /// Trimmed; empty means "run nothing".
    setup: String,
    teardown: worktree::TeardownPolicy,
}

impl SplitlaneApp {
    pub(crate) fn handle_new_worktree(
        &mut self,
        _: &crate::NewWorktree,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active_container_id())
        else {
            return;
        };
        self.open_worktree_dialog(ws_idx, window, cx);
    }

    /// Id of the container the action targets. Its own function because the
    /// rail's button knows an index and the action does not.
    fn active_container_id(&self) -> u64 {
        self.workspaces
            .get(self.active_idx)
            .map(|w| w.id)
            .unwrap_or(0)
    }

    /// Open the dialog for `ws_idx`. Refuses - with a toast, not silence -
    /// when the project is not a git repository: there is nothing to cut a
    /// worktree from, and an empty dialog would not say so.
    pub(crate) fn open_worktree_dialog(
        &mut self,
        ws_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.worktree_dialog.is_some() {
            if !self.worktree_dialog.as_ref().is_some_and(|wd| wd.running) {
                self.worktree_dialog = None;
                cx.notify();
            }
            return;
        }
        let Some(container) = self.workspaces.get(ws_idx) else {
            return;
        };
        let Some(repo_root) = container.repo_root.clone() else {
            self.show_toast("This project is not a git repository", cx);
            return;
        };
        let container_id = container.id;
        let based_on = if container.git_branch.is_empty() {
            "HEAD".to_string()
        } else {
            container.git_branch.clone()
        };

        let remembered_setup = container.worktree_setup.clone().unwrap_or_default();

        let branch_input = cx.new(|cx| TextInput::new("", "new-branch-name", cx));
        let branch_focus = branch_input.read(cx).focus_handle.clone();
        let setup_input =
            cx.new(|cx| TextInput::new(remembered_setup, "npm install, make setup, \u{2026}", cx));

        self.worktree_dialog = Some(WorktreeDialogState {
            container_id,
            repo_root: repo_root.clone(),
            based_on,
            branch_input,
            setup_input,
            claimed_dirs: Vec::new(),
            start_agent: true,
            teardown_auto: true,
            running: false,
            running_setup: false,
            error: None,
        });
        window.focus(&branch_focus, cx);
        cx.notify();

        // One `git worktree list`, off the render thread, so the Directory
        // row can name the hashed sibling when the slug path is taken.
        cx.spawn(async move |this, cx| {
            let claimed = smol::unblock(move || {
                worktree::list_worktrees(&repo_root)
                    .map(|entries| entries.into_iter().map(|entry| entry.path).collect())
                    .unwrap_or_else(|_| Vec::new())
            })
            .await;
            let _ = this.update(cx, |app, cx| {
                if let Some(wd) = app.worktree_dialog.as_mut()
                    && wd.container_id == container_id
                {
                    wd.claimed_dirs = claimed;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Close the dialog.
    ///
    /// Refused while **git** runs, because there is nothing on disk yet and
    /// closing would leave a half-made tree with nobody watching for its
    /// error. Allowed once the bootstrap is what is left: by then the tree
    /// exists, the project owns it and the session file says so, so closing
    /// costs nothing and the command runs to its end either way. It is a
    /// **detach, not a cancel** - the footer says as much while it is true,
    /// because a person who reads Esc as "stop that" and gets a session three
    /// minutes later has been misled by us.
    pub(crate) fn worktree_dialog_cancel(&mut self, cx: &mut Context<Self>) {
        if self
            .worktree_dialog
            .as_ref()
            .is_some_and(|wd| wd.running && !wd.running_setup)
        {
            return;
        }
        self.worktree_dialog = None;
        cx.notify();
    }

    fn worktree_dialog_set_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        if let Some(wd) = self.worktree_dialog.as_mut() {
            wd.running = false;
            wd.running_setup = false;
            wd.error = Some(message.into());
            cx.notify();
        }
    }

    /// The tree exists; what the dialog waits on now is the person's command.
    fn worktree_dialog_enter_setup(&mut self, container_id: u64, cx: &mut Context<Self>) {
        if let Some(wd) = self.worktree_dialog.as_mut()
            && wd.container_id == container_id
        {
            wd.running_setup = true;
            cx.notify();
        }
    }

    /// Validate the branch name and run git off the render thread. Nothing is
    /// executed when a guard fails - the error shows in the dialog and the
    /// form stays editable.
    pub(crate) fn worktree_dialog_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(wd) = self.worktree_dialog.as_ref() else {
            return;
        };
        if wd.running {
            return;
        }
        let container_id = wd.container_id;
        let repo_root = wd.repo_root.clone();
        let start_agent = wd.start_agent;
        let teardown = if wd.teardown_auto {
            worktree::TeardownPolicy::Auto
        } else {
            worktree::TeardownPolicy::Keep
        };
        let setup = wd.setup_input.read(cx).value().trim().to_string();
        let branch = wd.branch_input.read(cx).value().trim().to_string();

        if branch.is_empty() {
            self.worktree_dialog_set_error("Branch name is empty", cx);
            return;
        }
        // CWE-88: a leading '-' reads as a git flag downstream. Refused here
        // rather than trusted to quoting, exactly as the spec loader does.
        if branch.starts_with('-') {
            self.worktree_dialog_set_error("Branch name must not start with '-'", cx);
            return;
        }
        if worktree::branch_slug(&branch).is_empty() {
            self.worktree_dialog_set_error("Branch name has no filesystem-safe directory name", cx);
            return;
        }
        if !self.workspaces.iter().any(|w| w.id == container_id) {
            self.worktree_dialog_set_error("Project was closed", cx);
            return;
        }

        if let Some(wd) = self.worktree_dialog.as_mut() {
            wd.running = true;
            wd.running_setup = false;
            wd.error = None;
        }
        cx.notify();

        let plan = WorktreePlan {
            container_id,
            repo_root: repo_root.clone(),
            branch: branch.clone(),
            start_agent,
            setup: setup.clone(),
            teardown,
        };
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let created = smol::unblock(move || {
                    let (path, create_branch) = plan_new_worktree(&repo_root, &branch)?;
                    worktree::add_worktree(&repo_root, &path, &branch, create_branch)?;
                    // Best-effort by design: a partial copy is not a failure.
                    let _ = worktree::copy_env_files(&repo_root, &path);
                    Ok::<std::path::PathBuf, String>(path)
                })
                .await;

                let path = match created {
                    Ok(path) => path,
                    Err(e) => {
                        let _ = cx.update(|cx| {
                            this.update(cx, |app, cx| app.worktree_dialog_failed(e, cx))
                        });
                        return;
                    }
                };

                // **Ownership is recorded the moment the tree exists, not
                // when the bootstrap ends.** That window used to be the
                // milliseconds `git worktree add` takes; running `setup` in
                // front of the registration would widen it to the whole
                // timeout, and a quit inside it leaves a tree nobody owns -
                // on disk, and beyond the reach of the very checkbox the
                // person ticked. Whether setup succeeds says nothing about
                // who owns the tree.
                let attached = cx.update(|cx| {
                    this.update(cx, |app, cx| app.worktree_dialog_own(&plan, &path, cx))
                        .unwrap_or(false)
                });
                if !attached {
                    // The project closed while git ran, and `worktree_dialog_own`
                    // has said so. Nothing left to bootstrap for.
                    return;
                }

                // `setup` runs after the tree exists and BEFORE the agent
                // starts, which is the order `splitlane up` uses and the
                // reason the wait is worth having: an agent opening half-way
                // through an install would be looking at a tree nobody
                // finished. Same contract too - a failed bootstrap is
                // reported and never fatal, because the person can fix the
                // install in the pane it opens in.
                let setup_error = if plan.setup.is_empty() {
                    None
                } else {
                    let tree = path.clone();
                    let command = plan.setup.clone();
                    smol::unblock(move || {
                        worktree::run_setup(&tree, &command, worktree::DEFAULT_SETUP_TIMEOUT).err()
                    })
                    .await
                };

                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        app.worktree_dialog_finish(path, setup_error, plan, cx);
                    })
                });
            },
        )
        .detach();
    }

    /// Git failed: the error stays in the dialog, and nothing was created.
    fn worktree_dialog_failed(&mut self, message: String, cx: &mut Context<Self>) {
        if self.worktree_dialog.is_some() {
            self.worktree_dialog_set_error(message, cx);
        } else {
            self.show_toast(format!("New worktree: {message}"), cx);
        }
    }

    /// The tree exists: record ownership, remember the command, and name what
    /// the dialog waits on next. Returns whether the project is still open -
    /// `false` means the tree has nothing to attach to and the bootstrap has
    /// nobody to serve.
    ///
    /// Split out of the completion handler so that it runs **before** the
    /// bootstrap rather than after it; the call site says why the width of
    /// that window matters.
    fn worktree_dialog_own(
        &mut self,
        plan: &WorktreePlan,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|w| w.id == plan.container_id)
        else {
            // The project closed while git ran: the tree exists on disk with
            // nothing to attach it to. Say so rather than leak it silently.
            log::warn!(
                "worktree dialog: project closed during creation; {} left on disk",
                path.display()
            );
            self.show_toast(
                format!("Project closed - worktree left at {}", path.display()),
                cx,
            );
            self.worktree_dialog = None;
            cx.notify();
            return false;
        };

        // Ownership is registered FIRST so teardown parity holds even if
        // everything after it fails: the project owns this worktree exactly
        // like a `splitlane up` one.
        self.workspaces[ws_idx]
            .managed_worktrees
            .push(ManagedWorktree {
                path: path.to_path_buf(),
                repo_root: plan.repo_root.clone(),
                branch: plan.branch.clone(),
                teardown: plan.teardown,
            });

        // The command is the repository's, not this worktree's: remember it
        // so the next `+ worktree` here states it instead of asking again.
        // Written only on a creation that happened - a dialog that failed
        // keeps its text on screen and needs no memory of its own. Clearing
        // the field clears the memory, which is the same statement read the
        // other way round: the field says what runs here, and there is no
        // second, hidden answer standing behind it.
        self.workspaces[ws_idx].worktree_setup = worktree::remembered_setup(&plan.setup);

        // Persisted here for the same reason the record is pushed here: a
        // quit during `setup` must not cost the tree its owner.
        self.save_session(cx);

        if plan.setup.is_empty() {
            self.worktree_dialog = None;
        } else {
            self.worktree_dialog_enter_setup(plan.container_id, cx);
        }
        cx.notify();
        true
    }

    /// Everything after the bootstrap: the session, and what to say about it.
    fn worktree_dialog_finish(
        &mut self,
        path: std::path::PathBuf,
        setup_error: Option<String>,
        plan: WorktreePlan,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|w| w.id == plan.container_id)
        else {
            // Closed during `setup`. The ownership record went with the
            // container, so the tree is nobody's - the same outcome a
            // `splitlane up` container closing gives.
            log::warn!(
                "worktree dialog: project closed during setup; {} left on disk",
                path.display()
            );
            self.show_toast(
                format!("Project closed - worktree left at {}", path.display()),
                cx,
            );
            self.worktree_dialog = None;
            cx.notify();
            return;
        };

        self.worktree_dialog = None;

        // Which agent is the project's own answer, the same one the new-agent
        // chord uses. Where the project has none - it says "ask", or nobody has
        // ever chosen - this dialog does not guess one: it used to fall back to
        // Claude Code, so a person who had deliberately asked to be asked got a
        // different agent started without a word. The worktree is made either
        // way, and the toast says so.
        let agent = plan
            .start_agent
            .then(|| self.default_agent_for(ws_idx))
            .flatten();
        // A failed setup is said out loud even on the path that otherwise
        // stays silent: the tree is there, the agent is running in it, and
        // the install the person asked for did not happen.
        let setup_note = setup_error
            .map(|e| format!(" \u{2014} setup failed ({e})"))
            .unwrap_or_default();
        match agent {
            Some(agent) => {
                self.create_agent_session_in_cwd(
                    ws_idx,
                    agent,
                    plan.branch.clone(),
                    path.clone(),
                    cx,
                );
                if !setup_note.is_empty() {
                    self.show_toast(
                        format!("Worktree created at {}{setup_note}", path.display()),
                        cx,
                    );
                }
            }
            None if plan.start_agent => {
                self.show_toast(
                    format!(
                        "Worktree created at {}{setup_note} \u{2014} this project asks which agent, so none was started",
                        path.display()
                    ),
                    cx,
                );
            }
            None => {
                self.show_toast(
                    format!("Worktree created at {}{setup_note}", path.display()),
                    cx,
                );
            }
        }
        self.save_session(cx);
        cx.notify();
    }

    pub(crate) fn handle_worktree_dialog_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => self.worktree_dialog_cancel(cx),
            "enter" => self.worktree_dialog_confirm(cx),
            // Two fields, so `tab` has somewhere to go. It toggles rather
            // than cycles: with two, forward and backward are the same move,
            // which is why shift is not read.
            "tab" => self.worktree_dialog_toggle_field(window, cx),
            _ => {}
        }
    }

    /// Move the caret between `Branch name` and `Setup`.
    fn worktree_dialog_toggle_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wd) = self.worktree_dialog.as_ref() else {
            return;
        };
        if wd.running {
            return;
        }
        // focus-exact: this is a job about the keystroke that just happened -
        // which of the dialog's two fields has the caret right now.
        let branch = wd.branch_input.read(cx).focus_handle.clone();
        let setup = wd.setup_input.read(cx).focus_handle.clone();
        let next = if branch.is_focused(window) {
            setup
        } else {
            branch
        };
        window.focus(&next, cx);
        cx.notify();
    }

    pub(crate) fn render_worktree_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(wd) = self.worktree_dialog.as_ref() else {
            return div().into_any_element();
        };
        let ui = crate::theme::ui_colors();
        let running = wd.running;
        let branch = wd.branch_input.read(cx).value();
        let branch = branch.trim();
        // The directory is derived, not chosen: state it the moment the
        // branch name can produce one, and stay quiet until then.
        //
        // "Can produce one" is the same question the confirm guard asks. A
        // name with no ASCII in it slugs to nothing, and `worktree_dir` falls
        // back to a constant `branch` so that it stays a total function - but
        // that fallback is a safety net, not a directory this dialog will
        // create. Stating it would promise a path the next click refuses.
        let repo =
            crate::app::sidebar::collapse_home(&wd.repo_root.display().to_string(), &self.home_dir);
        let has_slug = !branch.is_empty() && !worktree::branch_slug(branch).is_empty();
        let directory: SharedString = if has_slug {
            // The same choice the planner makes: the slug path, unless a live
            // worktree already sits there.
            let slug_path = worktree::worktree_dir(&wd.repo_root, branch);
            let planned = if wd.claimed_dirs.contains(&slug_path) {
                worktree::worktree_dir_hashed(&wd.repo_root, branch)
            } else {
                slug_path
            };
            SharedString::from(crate::app::sidebar::collapse_home(
                &planned.display().to_string(),
                &self.home_dir,
            ))
        } else {
            SharedString::from(format!("{repo}.worktrees/\u{2026}"))
        };

        let field_label = |label: &'static str| {
            div()
                .text_size(tok::text::CAPTION)
                .text_color(ui.muted)
                .child(label)
        };
        let fact_row = |label: &'static str, value: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(tok::space::MD)
                .child(
                    div()
                        .flex_none()
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_right()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::PATH)
                        .text_color(ui.text_secondary)
                        .truncate()
                        .child(value),
                )
        };

        let start_agent_row = checkbox_row(
            "worktree-dialog-start-agent",
            "Start an agent session in it",
            wd.start_agent,
            running,
            |wd| wd.start_agent = !wd.start_agent,
            cx,
        );
        let teardown_row = checkbox_row(
            "worktree-dialog-teardown",
            "Remove it when the project closes",
            wd.teardown_auto,
            running,
            |wd| wd.teardown_auto = !wd.teardown_auto,
            cx,
        );

        let mut body = div()
            .flex()
            .flex_col()
            .gap(tok::space::MD)
            .px(tok::space::XXL)
            .py(tok::space::MD)
            .child(field_label("Branch name"))
            .child(
                div()
                    .border_1()
                    .border_color(ui.border)
                    .rounded(tok::radius::SMALL)
                    .px(tok::space::MD)
                    .py(tok::space::XS)
                    .child(wd.branch_input.clone()),
            )
            .child(fact_row(
                "Based on",
                SharedString::from(wd.based_on.clone()),
            ))
            .child(fact_row("Directory", directory))
            // The preset's `setup`, on this door. Its own label rather than a
            // placeholder alone, because a blank field with a grey hint in it
            // reads as optional decoration - and this one runs a command.
            .child(field_label("Setup"))
            .child(
                div()
                    .border_1()
                    .border_color(ui.border)
                    .rounded(tok::radius::SMALL)
                    .px(tok::space::MD)
                    .py(tok::space::XS)
                    .child(wd.setup_input.clone()),
            )
            .child(start_agent_row)
            .child(teardown_row)
            .child(
                div()
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted)
                    .child(
                        "Setup runs once, in the new tree. Sessions in a worktree are \
                         grouped under it, and a diff opened from one is scoped to that \
                         tree. A tree with uncommitted changes is never removed.",
                    ),
            );

        if let Some(err) = &wd.error {
            // git's failure verbatim - inert text, never parsed.
            body = body.child(
                div()
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.vc_deleted)
                    .child(err.clone()),
            );
        }

        let confirm_label: SharedString = if wd.running_setup {
            "Running setup…".into()
        } else if running {
            "Creating…".into()
        } else {
            "Create worktree".into()
        };
        let confirm_background = if running {
            ui.subtle
        } else {
            ui.accent.opacity(0.15)
        };
        let confirm_text = if running { ui.muted } else { ui.accent };
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(tok::space::XXL)
            .py(tok::space::MD)
            .border_t_1()
            .border_color(ui.border)
            .child(
                div()
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted)
                    .child(if wd.running_setup {
                        "Setup is running \u{b7} Esc leaves it to finish"
                    } else {
                        "Enter creates \u{b7} Esc cancels"
                    }),
            )
            .child(
                div()
                    .id("worktree-dialog-confirm")
                    .px(tok::space::XL)
                    .py(tok::space::XS)
                    .rounded(tok::radius::TAG)
                    .text_size(tok::text::ROW)
                    .bg(confirm_background)
                    .text_color(confirm_text)
                    .when(!running, |d| d.cursor_pointer())
                    .animated_hover(move |style, delta| {
                        let hovered_opacity = if running { 1.0 } else { 0.8 };
                        style.opacity(1.0 + (hovered_opacity - 1.0) * delta);
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.worktree_dialog_confirm(cx);
                        cx.stop_propagation();
                    }))
                    .child(confirm_label),
            );

        let project_name: SharedString = self
            .workspaces
            .iter()
            .find(|w| w.id == wd.container_id)
            .map(|w| SharedString::from(w.title.clone()))
            .unwrap_or_default();

        let card = div()
            .id("worktree-dialog")
            .occlude()
            .track_focus(&self.worktree_dialog_focus)
            .on_key_down(cx.listener(Self::handle_worktree_dialog_key_down))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.worktree_dialog_cancel(cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .w(px(520.))
            .flex()
            .flex_col()
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .rounded(tok::radius::WINDOW)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(tok::space::MD)
                    .px(tok::space::XXL)
                    .pt(tok::space::XL)
                    .pb(tok::space::XS)
                    .child(
                        div()
                            .text_size(tok::text::TITLE)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(ui.text)
                            .child("New worktree"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::LABEL)
                            .text_color(ui.dim)
                            .truncate()
                            .child(project_name),
                    ),
            )
            .child(body)
            .child(footer);

        deferred(
            div()
                .id("worktree-dialog-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(72.))
                .bg(ui.scrim)
                .child(card),
        )
        .with_priority(8)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_uses_hashed_path_when_slug_path_is_claimed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir(&repo_root).expect("repo dir");
        if !test_git(&repo_root, &["init"]) {
            return;
        }
        assert!(test_git(&repo_root, &["config", "core.autocrlf", "false"]));
        std::fs::write(repo_root.join("README.md"), "init\n").expect("readme");
        assert!(test_git(&repo_root, &["add", "README.md"]));
        assert!(test_git(
            &repo_root,
            &[
                "-c",
                "user.email=splitlane@example.com",
                "-c",
                "user.name=Splitlane",
                "commit",
                "-m",
                "init",
            ],
        ));

        let branch_a = "feat/a b";
        let branch_b = "feat/a-b";
        let legacy = worktree::worktree_dir(&repo_root, branch_a);
        std::fs::create_dir_all(legacy.parent().expect("worktree parent")).expect("parent dir");
        if !test_git(
            &repo_root,
            &[
                "worktree",
                "add",
                legacy.to_str().expect("utf8 path"),
                "-b",
                branch_a,
            ],
        ) {
            return;
        }

        let (path, create_branch) = plan_new_worktree(&repo_root, branch_b).expect("plan");
        assert_eq!(path, worktree::worktree_dir_hashed(&repo_root, branch_b));
        assert!(create_branch);
    }

    fn test_git(cwd: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }
}
