//! Lifecycle helpers + the content-area render branch for agent surfaces.
//!
//! There is no mode deciding what renders: `agents_target` names the selected
//! surface, and the main `render` calls [`SplitlaneApp::render_agents_main`]
//! only when that selection is an agent surface (a thread, the Skills page, or
//! the launcher). The area is terminal-only - a selected thread renders its
//! PTY.

use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;
use crate::{AgentsBranchMenuState, SplitlaneApp};
use gpui::{
    AppContext, ClickEvent, Context, Focusable, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, deferred, div,
    prelude::FluentBuilder, px, svg,
};
use splitlane_config::schema::TerminalSurfaceProfile;

/// The rail's width when the user has never said otherwise. The design's own
/// default; the user's own value overrides it and is persisted with the
/// session (`SplitlaneApp::rail_width`).
pub(crate) const RAIL_WIDTH: f32 = 272.0;
/// The range the drag handle allows. Narrower than the minimum and a session
/// name is a column of ellipses; wider than the maximum and the rail is
/// taking room from the thing it exists to navigate.
pub(crate) const RAIL_WIDTH_MIN: f32 = 224.0;
pub(crate) const RAIL_WIDTH_MAX: f32 = 400.0;
/// The hit area of the rail's right edge. Five pixels is the design's; it is
/// wider than the line it drags because a 1px target is not a target.
pub(crate) const RAIL_RESIZE_HIT: f32 = 5.0;

/// Branch-picker geometry, used to clamp the menu inside the window.
const BRANCH_MENU_WIDTH: f32 = 280.0;
/// Search row + label + the list's own `max_h`, rounded up.
const BRANCH_MENU_MAX_HEIGHT: f32 = 264.0;
const AGENTS_BRANCH_GIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const AGENTS_BRANCH_GIT_OUTPUT_CAP: u64 = 512 * 1024;
const AGENTS_EXITED_TERMINAL_CACHE_LIMIT: usize = 8;
const AGENTS_TERMINAL_CACHE_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

fn touch_lru(order: &mut Vec<u64>, id: u64) {
    order.retain(|existing| *existing != id);
    order.push(id);
}

fn oldest_evictable_terminal_id(
    order: &[u64],
    active: Option<u64>,
    cache_len: usize,
    limit: usize,
    mut is_evictable: impl FnMut(u64) -> bool,
) -> Option<u64> {
    if cache_len <= limit {
        return None;
    }
    order
        .iter()
        .copied()
        .find(|id| Some(*id) != active && is_evictable(*id))
}

/// Whether `source` is allowed to write this surface's name.
///
/// Two rules, and the second one is new. A **manual rename is
/// authoritative**: neither automatic source may clobber a deliberate label.
/// And a **stored summary never speaks over a running process**: the
/// `ai-title` backfill fires at every turn end, on a surface whose CLI is
/// repainting its own title all the while, so with both writing one field the
/// name the rail settled on was whichever had arrived last - while the pane
/// header, which reads the live title, went on showing the other one. One
/// surface, two names, neither of them wrong on its own.
///
/// The process wins while it is speaking, because it is saying what the agent
/// is doing *now* and it is what the header already shows. The summary keeps
/// the rows it was written for: a surface no process has named yet, which
/// after a restart is every restored row until its CLI paints.
fn may_name_surface(thread: &crate::project::Thread, source: TitleSource) -> bool {
    if thread.title_user_set {
        return false;
    }
    source == TitleSource::Process || !thread.title_from_process
}

/// Who is naming a surface. Two automatic sources write one field, and they do
/// not carry the same claim: [`TitleSource::Process`] is the CLI in the pane
/// saying what it is doing now, [`TitleSource::StoredSummary`] is the session's
/// `ai-title`, read back off disk at turn end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TitleSource {
    /// An OSC 0/2 title from the process in the pane.
    Process,
    /// The session store's own summary - the one `/resume` lists.
    StoredSummary,
}

impl SplitlaneApp {
    /// True while the content area is showing a container's panes - which is
    /// every moment the application layer is not up.
    ///
    /// It used to also ask whether a parked surface or the diff had taken the
    /// whole area instead. Nothing can: every surface is a pane's content now,
    /// so the only thing that replaces the panes is Settings.
    pub(crate) fn panes_surface_visible(&self) -> bool {
        self.settings_section.is_none()
    }

    /// Open the Skills browser (~/.claude/skills, ~/.codex/skills,
    /// ~/.agents/skills) in the application layer, beside Settings.
    ///
    /// It used to take the content area instead of a container's surfaces,
    /// which made it a fourth thing the selection could be pointing at and a
    /// surface with no kind for the targeting ladder to answer about. It
    /// describes the app, not a piece of work: nothing in it is per-pane, and
    /// it is attached to neither a project nor a session.
    pub(crate) fn show_agents_skills(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_settings_section(crate::SettingsSection::Skills, window, cx);
    }

    pub(crate) fn refresh_agents_skills(&mut self, cx: &mut Context<Self>) {
        if self.agents_view.agents_skills_loading {
            return;
        }
        self.agents_view.agents_skills_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let skills = smol::unblock(crate::agents_view::discover_skills).await;
            let _ = cx.update(|cx| {
                this.update(cx, |app, cx| {
                    app.agents_view.agents_skills = skills;
                    app.agents_view.agents_skills_loading = false;
                    cx.notify();
                })
            });
        })
        .detach();
    }

    /// Mark a skill id as "just copied" so its card label flips to
    /// "Copied". A scheduled task clears the slot iff it still holds the same
    /// id, so duplicate skill names do not collide.
    pub(crate) fn mark_skill_copied(&mut self, id: String, cx: &mut Context<Self>) {
        self.agents_view.agents_skills_copied = Some(id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(1500)).await;
            let _ = cx.update(|cx| {
                this.update(cx, |app, cx| {
                    if app.agents_view.agents_skills_copied.as_deref() == Some(id.as_str()) {
                        app.agents_view.agents_skills_copied = None;
                        cx.notify();
                    }
                })
            });
        })
        .detach();
    }

    /// The agent surface the user is looking at: the one in the focused pane,
    /// if that pane is showing one at all.
    ///
    /// It used to read the selection, because an agent surface was shown
    /// full-area and the selection was the whole of "which one". A surface is
    /// a pane's content now, so the question is answered where every other
    /// per-pane question is - by the pane that has the keyboard.
    pub(crate) fn current_thread_view_target(
        &self,
        cx: &gpui::App,
    ) -> Option<crate::project::AgentsTarget> {
        let ws_idx = self.active_idx;
        let root = self.workspaces.get(ws_idx)?.root.as_ref()?;
        let pane = self
            .focused_pane_now
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .filter(|pane| root.contains_leaf(pane))
            .or_else(|| root.first_leaf())?;
        let thread_id = pane
            .read(cx)
            .active_terminal_opt()
            .and_then(|view| view.read(cx).agent_thread_id)?;
        let thread_idx = self
            .workspaces
            .get(ws_idx)?
            .threads
            .iter()
            .position(|thread| thread.id == thread_id)?;
        Some(crate::project::AgentsTarget::Thread { ws_idx, thread_idx })
    }

    pub(crate) fn agents_environment_git_for_cwd(
        &self,
        cwd: &str,
    ) -> (String, bool, crate::workspace::GitDiffStats) {
        let cached = self.agents_view.agents_environment_git.get(cwd);
        let workspace = self
            .workspaces
            .iter()
            .find(|workspace| workspace.cwd.as_str() == cwd);
        let is_repo = workspace
            .map(|workspace| workspace.is_git_repo)
            .or_else(|| cached.map(|state| state.is_repo))
            .unwrap_or(false);
        let branch = if is_repo {
            workspace
                .and_then(|workspace| agents_environment_branch_label(&workspace.git_branch))
                .or_else(|| cached.and_then(|state| agents_environment_branch_label(&state.branch)))
                .unwrap_or_else(|| "Repository".to_string())
        } else {
            "No repository".to_string()
        };
        let git_stats = cached
            .map(|state| state.stats.clone())
            .or_else(|| workspace.map(|workspace| workspace.git_stats.clone()))
            .unwrap_or_default();
        (branch, is_repo, git_stats)
    }

    /// Open the branch picker for a container, from the container's own
    /// context menu. The branch is a property of the checkout, not of any one
    /// surface shown over it, so this is the affordance that owns it.
    pub(crate) fn open_agents_branch_menu_for_project(
        &mut self,
        ws_idx: usize,
        anchor: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspaces.get(ws_idx) else {
            return;
        };
        let cwd = workspace.cwd.clone();
        let (current, is_repo, _) = self.agents_environment_git_for_cwd(&cwd);
        if !is_repo {
            return;
        }
        self.open_agents_branch_menu(cwd, current, anchor, window, cx);
    }

    fn open_agents_branch_menu(
        &mut self,
        cwd: String,
        current: String,
        anchor: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cwd.trim().is_empty() {
            return;
        }
        let same_menu_open = self
            .agents_view
            .agents_branch_menu
            .as_ref()
            .is_some_and(|menu| menu.cwd == cwd);
        if same_menu_open {
            self.agents_view.agents_branch_menu = None;
            cx.notify();
            return;
        }

        let query_input =
            cx.new(|cx| crate::widgets::text_input::TextInput::new("", "Search branches", cx));
        cx.observe(&query_input, |_, _, cx| cx.notify()).detach();
        self.agents_view.agents_branch_menu = Some(AgentsBranchMenuState {
            cwd: cwd.clone(),
            current: current.clone(),
            branches: Vec::new(),
            loading: true,
            error: None,
            query_input: query_input.clone(),
            anchor,
        });
        query_input.read(cx).focus_handle.clone().focus(window, cx);
        cx.notify();

        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = smol::unblock({
                    let cwd = cwd.clone();
                    move || list_agents_environment_branches(&cwd)
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        let Some(menu) = app.agents_view.agents_branch_menu.as_mut() else {
                            return;
                        };
                        if menu.cwd != cwd {
                            return;
                        }
                        menu.loading = false;
                        match result {
                            Ok(branches) => {
                                menu.branches = branches;
                                menu.error = None;
                            }
                            Err(error) => {
                                menu.branches.clear();
                                menu.error = Some(error);
                            }
                        }
                        cx.notify();
                    })
                });
            },
        )
        .detach();
    }

    /// The open branch picker, rendered once at the app root. It belongs to a
    /// container rather than to any panel, so it lives next to the other
    /// context menus instead of inside whichever surface opened it.
    pub(crate) fn render_agents_branch_menu_overlay(
        &self,
        ui: crate::theme::UiColors,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let menu = self.agents_view.agents_branch_menu.clone()?;
        let (_, _, git_stats) = self.agents_environment_git_for_cwd(&menu.cwd);
        Some(render_agents_branch_menu(
            menu,
            git_stats.files_changed,
            ui,
            window,
            cx,
        ))
    }

    fn close_agents_branch_menu(
        &mut self,
        _: &gpui::MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agents_view.agents_branch_menu.take().is_some() {
            cx.notify();
        }
    }

    fn switch_agents_branch(
        &mut self,
        cwd: String,
        branch: String,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.agents_view.agents_branch_menu = None;
        self.focus_current_agents_terminal(window, cx);
        cx.notify();
        self.spawn_switch_branch(cwd, branch, cx);
    }

    /// Background `git switch` to an existing branch, then refresh the cached git
    /// state for every workspace/project rooted at `cwd`. Shared by the branch-row
    /// click and the search field's Enter-on-exact-match.
    fn spawn_switch_branch(&mut self, cwd: String, branch: String, cx: &mut Context<Self>) {
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = smol::unblock({
                    let cwd = cwd.clone();
                    let branch = branch.clone();
                    move || switch_agents_environment_branch(&cwd, &branch)
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| match result {
                        Ok((branch_now, is_repo, stats)) => {
                            app.apply_git_state_for_cwd(&cwd, branch_now, is_repo, stats);
                            app.show_toast(format!("Switched to {branch}"), cx);
                            cx.notify();
                        }
                        Err(error) => {
                            app.show_toast(format!("Couldn't switch to {branch}: {error}"), cx);
                        }
                    })
                });
            },
        )
        .detach();
    }

    /// Return keyboard focus to the active thread's terminal after the branch
    /// picker closes, so typing resumes in the PTY instead of landing on the
    /// dropped menu focus handle.
    fn focus_current_agents_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.current_thread_view_target(cx) else {
            return;
        };
        let Some(thread_id) = self.thread_for_target(target).map(|t| t.id) else {
            return;
        };
        if let Some(view) = self
            .agents_view
            .agents_terminal_view_cache
            .get(&thread_id)
            .cloned()
        {
            view.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    fn touch_agents_terminal_cache(&mut self, thread_id: u64) {
        touch_lru(&mut self.agents_view.agents_terminal_cache_lru, thread_id);
        self.agents_view
            .agents_terminal_cache_touched_at
            .insert(thread_id, std::time::Instant::now());
    }

    pub(crate) fn remove_agents_terminal_cache_entry(&mut self, thread_id: u64) {
        self.agents_view
            .agents_terminal_view_cache
            .remove(&thread_id);
        self.agents_view
            .agents_terminal_cache_lru
            .retain(|id| *id != thread_id);
        self.agents_view
            .agents_terminal_cache_touched_at
            .remove(&thread_id);
    }

    pub(crate) fn enforce_agents_terminal_cache_budget(
        &mut self,
        active_thread_id: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let cache_keys: std::collections::HashSet<u64> = self
            .agents_view
            .agents_terminal_view_cache
            .keys()
            .copied()
            .collect();
        self.agents_view
            .agents_terminal_cache_lru
            .retain(|id| cache_keys.contains(id));
        self.agents_view
            .agents_terminal_cache_touched_at
            .retain(|id, _| cache_keys.contains(id));

        // V1 fallback for live scrollback trim: dropping a live TerminalView
        // terminates its PTY, so the TTL path only releases exited terminals.
        let now = std::time::Instant::now();
        let expired: Vec<u64> = self
            .agents_view
            .agents_terminal_cache_lru
            .iter()
            .copied()
            .filter(|id| Some(*id) != active_thread_id)
            .filter(|id| {
                self.agents_view
                    .agents_terminal_cache_touched_at
                    .get(id)
                    .is_some_and(|last| now.duration_since(*last) >= AGENTS_TERMINAL_CACHE_IDLE_TTL)
            })
            .filter(|id| {
                self.agents_view
                    .agents_terminal_view_cache
                    .get(id)
                    .is_some_and(|view| view.read(cx).terminal.exited.is_some())
            })
            .collect();
        for thread_id in expired {
            self.remove_agents_terminal_cache_entry(thread_id);
        }

        while self.agents_view.agents_terminal_view_cache.len() > AGENTS_EXITED_TERMINAL_CACHE_LIMIT
        {
            let lru = self.agents_view.agents_terminal_cache_lru.clone();
            let cache_len = self.agents_view.agents_terminal_view_cache.len();
            let evict = oldest_evictable_terminal_id(
                &lru,
                active_thread_id,
                cache_len,
                AGENTS_EXITED_TERMINAL_CACHE_LIMIT,
                |id| {
                    self.agents_view
                        .agents_terminal_view_cache
                        .get(&id)
                        .is_some_and(|view| view.read(cx).terminal.exited.is_some())
                },
            );
            let Some(thread_id) = evict else {
                log::debug!(
                    "agents terminal cache remains over budget; active/running terminals are protected"
                );
                break;
            };
            self.remove_agents_terminal_cache_entry(thread_id);
        }
    }

    /// Keyboard handling for branch-picker commands that bubble out of the
    /// focused `TextInput`: Enter switches to an exact match, Escape dismisses.
    pub(crate) fn handle_agents_branch_menu_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agents_view.agents_branch_menu.is_none() {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                self.agents_view.agents_branch_menu = None;
                self.focus_current_agents_terminal(window, cx);
                cx.notify();
            }
            "enter" => {
                // With the create action removed, Enter only switches to an exact
                // match; a non-matching query is a no-op (keep filtering / click).
                let resolved = {
                    let Some(menu) = self.agents_view.agents_branch_menu.as_ref() else {
                        return;
                    };
                    let name = menu.query_input.read(cx).value().trim().to_string();
                    if name.is_empty() || !menu.branches.contains(&name) {
                        return;
                    }
                    (menu.cwd.clone(), name)
                };
                let (cwd, name) = resolved;
                self.agents_view.agents_branch_menu = None;
                self.focus_current_agents_terminal(window, cx);
                cx.notify();
                self.spawn_switch_branch(cwd, name, cx);
            }
            _ => {}
        }
    }

    pub(crate) fn spawn_agents_environment_git_refresh(
        &mut self,
        cwd: String,
        cx: &mut Context<Self>,
    ) {
        let cwd = cwd.trim().to_string();
        if cwd.is_empty() {
            return;
        }
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let (branch, is_repo, stats) = smol::unblock({
                    let cwd = cwd.clone();
                    move || read_agents_environment_git_state(&cwd)
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        if app.apply_git_state_for_cwd(&cwd, branch, is_repo, stats) {
                            cx.notify();
                        }
                    })
                });
            },
        )
        .detach();
    }

    pub(crate) fn apply_git_state_for_cwd(
        &mut self,
        cwd: &str,
        branch: String,
        is_repo: bool,
        stats: crate::workspace::GitDiffStats,
    ) -> bool {
        let mut changed = false;
        let state = crate::AgentsGitState {
            branch: branch.clone(),
            is_repo,
            stats: stats.clone(),
        };
        if self.agents_view.agents_environment_git.get(cwd) != Some(&state) {
            self.agents_view
                .agents_environment_git
                .insert(cwd.to_string(), state);
            changed = true;
        }
        for workspace in &mut self.workspaces {
            if workspace.cwd == cwd {
                if workspace.git_branch != branch {
                    workspace.git_branch = branch.clone();
                    changed = true;
                }
                if workspace.is_git_repo != is_repo {
                    workspace.is_git_repo = is_repo;
                    changed = true;
                }
                if workspace.git_stats != stats {
                    workspace.git_stats = stats.clone();
                    changed = true;
                }
            }
        }
        changed
    }

    /// Resolve a center target to its backing [`Thread`], whether it lives
    /// in a container. `None` when the
    /// target is out of range.
    pub(crate) fn thread_for_target(
        &self,
        target: crate::project::AgentsTarget,
    ) -> Option<&crate::project::Thread> {
        use crate::project::AgentsTarget;
        match target {
            AgentsTarget::Thread { ws_idx, thread_idx } => {
                self.workspaces.get(ws_idx)?.threads.get(thread_idx)
            }
            AgentsTarget::Panes { .. } | AgentsTarget::Diff { .. } => None,
        }
    }

    /// Mount (or reuse from cache) the [`TerminalView`] entity that
    /// backs a Terminal Thread at `target`. Returns the entity ready
    /// to be wrapped by [`render_terminal_thread_surface`].
    ///
    /// Cache hit re-binds the existing entity so the running shell
    /// process survives sidebar navigation; cache miss spawns a fresh
    /// PTY in the thread's cwd via [`TerminalView::with_cwd`] and (when
    /// the thread is bound to a CLI agent) auto-launches it.
    ///
    /// `workspace_id` for the new view is the thread's own `id`, raw. Ids
    /// come from one process-wide counter, so a thread id and a workspace id
    /// can never name the same thing and the `ai.*` hook frames emitted from
    /// inside this PTY route straight back to the thread (spinner / attention
    /// state).
    pub(crate) fn mount_agents_terminal_for_target(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Entity<crate::terminal::view::TerminalView>> {
        // Resolve the target to its
        // backing Thread. The cache key below is the stable `Thread::id`, so
        // warm-resume survives navigation between surfaces.
        let thread = self.thread_for_target(target)?;
        let thread_id = thread.id;
        // A surface showing in a slot is authoritative over the cache: the
        // cache can drop an exited terminal, and building a second view for a
        // surface that is on screen would put two PTYs behind one row - the
        // second one unbound, because the first still holds the session.
        if let Some(view) = self
            .workspaces
            .get(target.ws_idx())
            .and_then(|container| container.root.as_ref())
            .and_then(|root| crate::app::agent_slots::agent_pane(root, thread_id, cx))
            .and_then(|pane| crate::app::agent_slots::agent_view_in_pane(&pane, thread_id, cx))
        {
            return Some(view);
        }
        if let Some(cached) = self
            .agents_view
            .agents_terminal_view_cache
            .get(&thread_id)
            .cloned()
        {
            self.touch_agents_terminal_cache(thread_id);
            self.enforce_agents_terminal_cache_budget(Some(thread_id), cx);
            return Some(cached.clone());
        }
        // This branch is the one that issues the launch command - the cached
        // and on-screen branches above return a PTY that is already running -
        // so this is the moment the design puts `starting` on the record. Only
        // for a surface that launches something: a bare shell gets no command
        // and is not starting anything.
        let launches = thread.terminal_agent.is_some();
        let view = Self::build_agent_terminal_view(thread, &self.cached_config, cx);
        if launches {
            self.mark_surface_starting(thread_id, &view, cx);
        }
        self.agents_view
            .agents_terminal_view_cache
            .insert(thread_id, view.clone());
        self.touch_agents_terminal_cache(thread_id);
        self.enforce_agents_terminal_cache_budget(Some(thread_id), cx);
        Some(view)
    }

    /// Take an already-running terminal into the cache under `thread_id`.
    ///
    /// The inverse of [`Self::mount_agents_terminal_for_target`], which builds
    /// a view for a surface record. Here the view exists and the record has
    /// just been made for it - a terminal that was living in a pane and
    /// nowhere else, about to be pushed out of it. Without this the pane's
    /// pointer was the only one, and dropping it took the PTY with it.
    pub(crate) fn adopt_terminal_as_surface(
        &mut self,
        thread_id: u64,
        view: gpui::Entity<crate::terminal::view::TerminalView>,
        cx: &mut Context<Self>,
    ) {
        self.agents_view
            .agents_terminal_view_cache
            .insert(thread_id, view);
        self.touch_agents_terminal_cache(thread_id);
        self.enforce_agents_terminal_cache_budget(Some(thread_id), cx);
    }

    /// Build the PTY view for an agent surface: the terminal, its launch
    /// command resolved through the mint-vs-resume contract, and the title
    /// subscription.
    ///
    /// An associated function rather than a method because session restore
    /// builds these while the containers are still being assembled - they are
    /// not in `self.workspaces` yet, and an agent showing in a restored slot
    /// has to come back with its binding at that moment, not on some later
    /// selection.
    pub(crate) fn build_agent_terminal_view(
        thread: &crate::project::Thread,
        config: &splitlane_config::schema::SplitlaneConfig,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<crate::terminal::view::TerminalView> {
        // **The one place macOS gets asked about notifications**, and it is
        // here because this is the moment the app's four notifications first
        // become possible: every one of them is about an agent session, and
        // both doors onto a session - somebody starting one, and restore
        // bringing one back - come through this function. Asking at app launch
        // would put a system prompt in front of somebody who has not yet done
        // the thing notifications are about; asking when the first notification
        // wants to fire would put it in front of nobody, because that is by
        // construction a moment the person is not looking.
        //
        // A shell surface is not a session in this sense and does not ask.
        #[cfg(target_os = "macos")]
        if thread.terminal_agent.is_some() {
            // Off the render thread without exception: this blocks for as long
            // as the system alert is on screen.
            cx.background_executor()
                .spawn(async {
                    smol::unblock(crate::agents::mac_notifications::request_if_never_asked).await;
                })
                .detach();
        }
        let thread_id = thread.id;
        let cwd = std::path::PathBuf::from(&thread.cwd);
        // The thread's forced agent session id (Claude only), spliced into
        // the launch command below so the live PTY binds 1:1 to its on-disk
        // session file (and resumes the same session after a restart).
        let bound_session = thread.session_id.clone();
        // Resolved against the on-disk session store below: the first launch
        // mints the id, every reopen reattaches to it. See
        // [`crate::agent_launcher::SessionBinding`].
        let thread_cwd = thread.cwd.clone();
        // Explicit per-thread agent wins; legacy `Agent`-kind chat rows
        // fall back to their stored ACP agent so reopening them launches
        // the same CLI in a terminal. Plain Terminal Threads stay a bare
        // shell (`None`).
        let terminal_agent = thread.terminal_agent.or_else(|| match thread.kind {
            crate::project::ThreadKind::Agent => Some(
                crate::agent_launcher::TerminalAgent::from_agent_kind(thread.agent),
            ),
            crate::project::ThreadKind::Terminal => None,
        });
        let view = cx.new(|cx| {
            let mut view = crate::terminal::view::TerminalView::with_cwd_and_profile(
                thread_id,
                Some(cwd),
                None,
                TerminalSurfaceProfile::Agent,
                cx,
            );
            // The surface's identity, carried by the PTY itself so it survives
            // being moved into a slot, split, or dragged to another pane.
            view.agent_thread_id = Some(thread_id);
            // And whether it is an agent at all: the container's own shells
            // are mounted through this same builder and must keep being named
            // the way a shell is named.
            view.surface_is_agent = !crate::app::agents_sidebar::is_shell_surface(thread);
            // And its name, when the record carries one a person typed. The
            // rail names a surface from the record and the slot header names
            // it from this field; a rename writes both, but a surface parked
            // long enough to fall out of the view cache is rebuilt here, and
            // rebuilding it without the name brought the divergence back on
            // the next mount. An auto-derived record title is deliberately
            // NOT copied - that one the header derives better and keeps
            // current.
            if thread.title_user_set && !thread.title.trim().is_empty() {
                view.terminal.custom_name = Some(thread.title.clone());
            }
            // And the record's name as the floor under the header, for the
            // window before the CLI reports a title of its own. Not
            // `custom_name`: that would pin it, and the header is supposed to
            // follow the process once the process speaks.
            view.record_title = Some(thread.title.clone());
            view
        });
        // First mount of this thread's PTY (fresh creation or first reopen
        // after a restart). When the thread is bound to a CLI agent, auto-run
        // its launch command so opening the thread drops the user straight
        // into the agent. The command honors
        // `claude_code_bypass_permissions` via `launch_command`. Writing
        // immediately is safe even though `with_cwd` opens the PTY on a
        // background thread: `send_command` → `write_to_pty`
        // buffers into the display-only terminal's `pending_input` queue and
        // `TerminalState::promote` flushes it the moment the PTY goes live,
        // so the command is never dropped on the pre-promotion race.
        // Callers reuse a cached view for in-session re-selection, so a
        // running agent is never relaunched on navigation.
        if let Some(agent) = terminal_agent {
            let binding = crate::agent_launcher::SessionBinding::resolve(
                bound_session.as_deref(),
                &thread_cwd,
            );
            let cmd = agent.launch_command_with_session(config, binding);
            view.read(cx).send_command(&cmd);
        }
        // Mirror Zed's `AgentTerminal::refresh_terminal_metadata`
        // (agent_panel.rs around `TerminalEvent::TitleChanged`): every
        // OSC 0/2 title update from the running process is reflected
        // into the sidebar row label. That's what lets a `claude`
        // session inside a Terminal Thread surface its auto-summary
        // ("Refactor auth middleware") in the sidebar instead of the
        // generic "Terminal" placeholder. The subscription is detached
        // -- the entity owns its lifecycle and the listener drops with
        // it when the cache evicts the entry.
        cx.subscribe(
            &view,
            move |this, src, event: &crate::terminal::view::TerminalEvent, cx| {
                if let crate::terminal::view::TerminalEvent::TitleChanged = event {
                    let new_title = src.read(cx).terminal.title.clone();
                    this.handle_terminal_thread_title_changed(
                        thread_id,
                        new_title,
                        TitleSource::Process,
                        cx,
                    );
                }
            },
        )
        .detach();
        view
    }

    /// Put `starting` on a surface whose launch command has just been issued,
    /// and give the output counter a baseline to measure it against.
    ///
    /// **The baseline is what makes the word end at the first output rather
    /// than at the second pass.** `deposit_pty_flow` can only read a counter as
    /// a difference, so with no previous value it has nothing to compare and
    /// the word survives that pass. Seeding it here with the PTY's count *at
    /// the moment of launch* is what the word's scope literally asks for - "until the
    /// first output from that pane" - and it is the only form that is right for
    /// a **relaunch** into a pane whose counter is already high: measured from
    /// zero, the first pass would read old bytes as new output and take the
    /// word down before the new process had painted anything.
    fn mark_surface_starting(
        &mut self,
        thread_id: u64,
        view: &gpui::Entity<crate::terminal::view::TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let baseline = view.read(cx).terminal.output_generation;
        self.pty_flow.insert(
            thread_id,
            crate::app::agent_state_pass::PtyFlow {
                generation: baseline,
                // Not ours: `starting` is the launch's claim, not this
                // source's. All that source does is take it back down.
                ours: false,
            },
        );
        if let Some(thread) = self.agents_thread_mut_by_id(thread_id) {
            thread.status = crate::project::ThreadStatus::Starting;
        } else {
            // The write is an invariant, not an optimisation: a surface that
            // launched and is not in the list is a bug somewhere above.
            tracing::warn!(
                target: "splitlane_app::launcher",
                thread_id,
                "launched a surface with no record to mark as starting"
            );
        }
    }

    /// Resolve an agent surface by its stable [`crate::project::Thread::id`].
    pub(crate) fn agents_thread_mut_by_id(
        &mut self,
        thread_id: u64,
    ) -> Option<&mut crate::project::Thread> {
        self.workspaces
            .iter_mut()
            .flat_map(|p| p.threads.iter_mut())
            .find(|t| t.id == thread_id)
    }

    /// Open the overflow menu for the *selected* surface, anchored just
    /// below the title bar. A no-op outside Agents mode or when nothing is
    /// selected. The menu reuses `agents_menu_open` so click-outside-to-close
    /// and the deferred render path are shared with the right-click menus.
    ///
    /// Originally a title-bar `⋯` button, which `fdaa764` removed as
    /// unreachable chrome. This is now the keyboard/palette path; the mouse
    /// path is the per-row `⋯` in the sidebar, which opens the menu for
    /// the row under the cursor rather than for the selection.
    pub(crate) fn handle_open_agents_thread_menu(
        &mut self,
        _: &crate::OpenAgentsThreadMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The keyboard path names what the focused pane is showing - the
        // same surface its own `⋯` would open the menu for.
        let target =
            self.current_thread_view_target(cx)
                .unwrap_or(crate::project::AgentsTarget::Panes {
                    ws_idx: self.active_idx,
                });
        // Anchor below the title bar near the brand slot. `render_open_agents_menu`
        // clamps to the window bounds if it would overflow the bottom.
        let position = gpui::point(px(12.), px(40.));
        let menu = match target {
            crate::project::AgentsTarget::Thread { ws_idx, thread_idx } => {
                crate::app::agents_sidebar::AgentsContextMenu::Thread {
                    ws_idx,
                    thread_idx,
                    position,
                    // The keyboard path names the selection, and what the
                    // selection puts on screen is the slot - so it opens the
                    // same menu the slot's own overflow button does.
                    origin: crate::app::agents_sidebar::MenuOrigin::SlotHeader,
                }
            }
            // Panes and Diff are not surfaces with a menu of their own yet;
            // what the selection names in both cases is the container, so the
            // keyboard opens the container's menu - the same one its row opens
            // on right-click. There used to be a second renderer here for the
            // same object, left behind when the two container menus merged;
            // nothing could reach it, because the guard above returned early
            // for exactly these two selections.
            crate::project::AgentsTarget::Panes { ws_idx }
            | crate::project::AgentsTarget::Diff { ws_idx } => {
                self.open_agents_project_menu(ws_idx, position, cx);
                return;
            }
        };
        if self.thread_for_target(target).is_none() {
            return;
        }
        self.cancel_agents_rename(cx);
        self.agents_view.agents_menu_open = Some(menu);
        cx.notify();
    }

    /// React to an OSC-driven title update from the PTY backing a
    /// Terminal Thread. Updates the matching sidebar row's title and
    /// persists the session so the new label survives a restart.
    ///
    /// Skips two cases on purpose:
    /// 1. Empty / whitespace-only titles -- some shells emit a stray
    ///    blank `ESC]0;\x07` on startup before the real prompt loads.
    /// 2. The literal `"Terminal"` fallback alacritty stamps after a
    ///    `ResetTitle` OSC, so a child shell exiting (e.g. `claude`
    ///    completing a session) does not wipe the meaningful
    ///    process-reported title with a generic placeholder.
    pub(crate) fn handle_terminal_thread_title_changed(
        &mut self,
        thread_id: u64,
        new_title: String,
        source: TitleSource,
        cx: &mut Context<Self>,
    ) {
        // Strips whitespace + leading spinner/bullet glyphs (Codex
        // braille, Claude Code pinwheel, generic `●`/`•`). Returns
        // `None` if nothing meaningful is left.
        let Some(normalized) = crate::project::clean_sidebar_title(&new_title) else {
            return;
        };
        if normalized == "Terminal" {
            // Don't let alacritty's `ResetTitle` fallback wipe a
            // meaningful process-reported title once a child shell
            // exits and the title resets to the default.
            return;
        }
        for project in self.workspaces.iter_mut() {
            if let Some(thread) = project.threads.iter_mut().find(|t| t.id == thread_id) {
                if !may_name_surface(thread, source) {
                    return;
                }
                if source == TitleSource::Process {
                    thread.title_from_process = true;
                }
                if thread.title == normalized {
                    return;
                }
                thread.title = normalized;
                self.save_session(cx);
                cx.notify();
                return;
            }
        }
    }

    /// Adopt the live agent session's LLM `ai-title` as the thread's sidebar
    /// label at turn end - the same summary `/resume` surfaces. Reads the
    /// on-disk session store off the main thread, then picks only the bound
    /// session created by the thread's forced session id. Agents without a
    /// forced id intentionally skip this path: cwd-newest matching can rename
    /// the wrong thread when several agents share a repository.
    pub(crate) fn spawn_thread_title_backfill(
        &self,
        thread_id: u64,
        cwd: String,
        agent: crate::agent_sessions::SessionAgent,
        bound_session: String,
        cx: &mut Context<Self>,
    ) {
        if cwd.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let sessions = smol::unblock(move || read_sessions_for(agent, &cwd)).await;
            if let Some(summary) = title_summary_for_bound_session(sessions, &bound_session) {
                let _ = this.update(cx, |app, cx| {
                    app.handle_terminal_thread_title_changed(
                        thread_id,
                        summary,
                        TitleSource::StoredSummary,
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    // Sidebar render branch for [`AppMode::Agents`] now lives in
    // [`crate::app::agents_sidebar`], which replaced the
    // placeholder that used to be shipped here.
}

/// Dispatch a cwd-scoped session scan to the matching on-disk reader.
/// **Blocking I/O** - call from inside `smol::unblock`.
fn read_sessions_for(
    agent: crate::agent_sessions::SessionAgent,
    cwd: &str,
) -> Vec<crate::agent_sessions::SessionMeta> {
    crate::agent_sessions::read_sessions_for_cwd(agent, cwd)
}

fn title_summary_for_bound_session(
    sessions: Vec<crate::agent_sessions::SessionMeta>,
    bound_session: &str,
) -> Option<String> {
    sessions
        .into_iter()
        .find(|s| s.session_id == bound_session)
        .and_then(|s| s.summary)
        .filter(|summary| !summary.is_empty())
}

fn render_agents_branch_menu(
    menu_state: AgentsBranchMenuState,
    files_changed: usize,
    ui: crate::theme::UiColors,
    window: &Window,
    cx: &mut Context<SplitlaneApp>,
) -> gpui::AnyElement {
    let cwd = menu_state.cwd.clone();
    let query_input = menu_state.query_input.clone();
    let query = query_input.read(cx).value();
    let query_lc = query.trim().to_lowercase();

    let mut menu =
        crate::settings::components::menu_surface(div().id("agents-env-branch-menu"), ui)
            .flex()
            .flex_col()
            .gap(tok::space::XS)
            .p(tok::space::XS)
            .w(px(280.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(SplitlaneApp::close_agents_branch_menu))
            .on_key_down(cx.listener(SplitlaneApp::handle_agents_branch_menu_key_down))
            .child(render_agents_branch_search_row(query_input, ui));

    if menu_state.loading {
        menu = menu.child(render_agents_branch_menu_status("Loading branches…", ui));
    } else if let Some(error) = menu_state.error {
        menu = menu.child(render_agents_branch_menu_status(error, ui));
    } else {
        menu = menu.child(
            div()
                .px(tok::space::MD)
                .pt(tok::space::XS)
                .pb(tok::space::XS)
                .text_size(tok::text::CAPTION)
                .text_color(ui.muted)
                .child("Branches"),
        );

        let filtered: Vec<String> = menu_state
            .branches
            .iter()
            .filter(|branch| query_lc.is_empty() || branch.to_lowercase().contains(&query_lc))
            .cloned()
            .collect();

        if filtered.is_empty() {
            menu = menu.child(render_agents_branch_menu_status("No branches", ui));
        } else {
            let mut list = div()
                .id("agents-env-branch-list")
                .flex()
                .flex_col()
                .gap(px(1.))
                .max_h(px(200.))
                .overflow_y_scroll();
            for (idx, branch) in filtered.into_iter().enumerate() {
                let selected = branch == menu_state.current;
                list = list.child(render_agents_branch_item(
                    idx,
                    branch,
                    selected,
                    if selected { files_changed } else { 0 },
                    cwd.clone(),
                    ui,
                    cx,
                ));
            }
            menu = menu.child(list);
        }
    }

    // Anchored where it was opened from, clamped to the window like every
    // other context menu. It used to hang off the environment card at a fixed
    // offset, which is exactly why only that card could open it.
    let menu_pos = crate::app::sidebar::context_menu::clamped_context_menu_position(
        menu_state.anchor,
        px(BRANCH_MENU_WIDTH),
        px(BRANCH_MENU_MAX_HEIGHT),
        window,
    );
    deferred(
        div()
            .absolute()
            .left(menu_pos.x)
            .top(menu_pos.y)
            .occlude()
            .child(menu),
    )
    .with_priority(3)
    .into_any_element()
}

/// Branch-picker search field. Editing is handled by `TextInput`, while the
/// parent menu handles only Enter/Escape after those keys bubble.
fn render_agents_branch_search_row(
    query_input: gpui::Entity<crate::widgets::text_input::TextInput>,
    ui: crate::theme::UiColors,
) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::MD)
        .px(tok::space::MD)
        .h(tok::row::SEARCH)
        .child(
            svg()
                .size(px(12.))
                .flex_none()
                .path("icons/tool_search.svg")
                .text_color(ui.muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_x_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(tok::text::ROW)
                .text_color(ui.text)
                .child(query_input.into_any_element()),
        )
        .into_any_element()
}

/// One branch row: leading branch glyph, the name, an optional "Uncommitted: N
/// files" sub-label on the checked-out branch, and a trailing check.
fn render_agents_branch_item(
    idx: usize,
    branch: String,
    selected: bool,
    files_changed: usize,
    cwd: String,
    ui: crate::theme::UiColors,
    cx: &mut Context<SplitlaneApp>,
) -> gpui::AnyElement {
    let item_branch = branch.clone();
    let selected_background = crate::settings::components::with_alpha(ui.text, 0.10);
    let resting_background = if selected {
        selected_background
    } else {
        crate::settings::components::with_alpha(ui.text, 0.0)
    };
    let hover_background = if selected {
        selected_background
    } else {
        crate::settings::components::with_alpha(ui.text, 0.05)
    };
    let row = div()
        .id(SharedString::from(format!("agents-env-branch-{idx}")))
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::MD)
        .px(tok::space::MD)
        .py(tok::space::XS)
        .rounded(tok::radius::PANEL)
        .bg(resting_background)
        .animated_hover_bg(resting_background, hover_background)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .cursor_pointer()
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            this.switch_agents_branch(cwd.clone(), item_branch.clone(), event, window, cx);
        }));
    row.child(
        svg()
            .size(px(12.))
            .flex_none()
            .path("icons/git-branch.svg")
            .text_color(ui.muted),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                div()
                    .overflow_x_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(tok::text::ROW)
                    .text_color(ui.text)
                    .child(branch),
            )
            .when(files_changed > 0, |d| {
                d.child(
                    div()
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.muted)
                        .child(format!(
                            "Uncommitted: {files_changed} file{}",
                            if files_changed > 1 { "s" } else { "" }
                        )),
                )
            }),
    )
    .child(div().w(px(14.)).flex_none().child(if selected {
        svg()
            .size(px(12.))
            .path("icons/check.svg")
            .text_color(ui.text)
            .into_any_element()
    } else {
        div().size(px(14.)).into_any_element()
    }))
    .into_any_element()
}

fn render_agents_branch_menu_status(
    label: impl Into<String>,
    ui: crate::theme::UiColors,
) -> gpui::AnyElement {
    div()
        .h(px(28.))
        .px(tok::space::MD)
        .flex()
        .items_center()
        .text_size(tok::text::ROW)
        .text_color(ui.muted)
        .child(label.into())
        .into_any_element()
}

/// A non-empty branch name, or `None` when the checkout reports none.
fn agents_environment_branch_label(branch: &str) -> Option<String> {
    let branch = branch.trim();
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    }
}

fn list_agents_environment_branches(cwd: &str) -> Result<Vec<String>, String> {
    let mut command = std::process::Command::new("git");
    command
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0");

    let output = splitlane_process::run_with_timeout(
        command,
        AGENTS_BRANCH_GIT_DEADLINE,
        AGENTS_BRANCH_GIT_OUTPUT_CAP,
    )
    .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(git_output_error(&output));
    }

    let mut branches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    branches.sort();
    branches.dedup();
    Ok(branches)
}

fn read_agents_environment_git_state(cwd: &str) -> (String, bool, crate::workspace::GitDiffStats) {
    let (branch, is_repo) = crate::workspace::detect_branch(cwd);
    let stats = crate::workspace::GitDiffStats::from_cwd(cwd);
    (branch, is_repo, stats)
}

fn switch_agents_environment_branch(
    cwd: &str,
    branch: &str,
) -> Result<(String, bool, crate::workspace::GitDiffStats), String> {
    let mut command = std::process::Command::new("git");
    command
        .args(["switch", "--", branch])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0");

    let output = splitlane_process::run_with_timeout(
        command,
        AGENTS_BRANCH_GIT_DEADLINE,
        AGENTS_BRANCH_GIT_OUTPUT_CAP,
    )
    .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(git_output_error(&output));
    }

    Ok(read_agents_environment_git_state(cwd))
}

fn git_output_error(output: &splitlane_process::BoundedOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    if message.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        message.lines().next().unwrap_or(message).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_lru_moves_existing_id_to_back() {
        let mut order = vec![1, 2, 3];
        touch_lru(&mut order, 2);
        assert_eq!(order, vec![1, 3, 2]);
    }

    #[test]
    fn oldest_evictable_terminal_id_protects_active_and_running() {
        let order = vec![1, 2, 3, 4];
        let exited = std::collections::HashSet::from([1, 2, 4]);

        let evict = oldest_evictable_terminal_id(&order, Some(1), 9, 8, |id| exited.contains(&id));
        assert_eq!(
            evict,
            Some(2),
            "oldest active entry is protected, next exited entry is evicted"
        );

        let evict = oldest_evictable_terminal_id(&order, Some(2), 9, 8, |id| id == 3);
        assert_eq!(
            evict,
            Some(3),
            "a running active entry is skipped but an evictable inactive entry can drop"
        );

        let evict = oldest_evictable_terminal_id(&order, None, 8, 8, |_| true);
        assert_eq!(evict, None, "cache at budget does not evict");
    }

    #[test]
    fn title_summary_for_bound_session_requires_exact_id_match() {
        let sessions = vec![
            session_meta("newest", "wrong title"),
            session_meta("bound", "right title"),
        ];

        assert_eq!(
            title_summary_for_bound_session(sessions, "bound").as_deref(),
            Some("right title"),
            "title backfill must not choose the newest cwd session"
        );
    }

    #[test]
    fn title_summary_for_bound_session_drops_missing_or_empty_summary() {
        let mut empty = session_meta("bound", "");
        empty.summary = Some(String::new());
        let missing = session_meta("other", "other title");

        assert_eq!(
            title_summary_for_bound_session(vec![missing], "bound"),
            None
        );
        assert_eq!(title_summary_for_bound_session(vec![empty], "bound"), None);
    }

    #[test]
    fn a_stored_summary_never_speaks_over_a_running_process() {
        use crate::project::Thread;
        let mut thread = Thread::new_terminal("session", "/repo", None);

        // Nothing has named it yet: either source may.
        assert!(may_name_surface(&thread, TitleSource::Process));
        assert!(may_name_surface(&thread, TitleSource::StoredSummary));

        // Once the process has spoken, the backfill stops competing. This is
        // the defect: both wrote one field at every turn end, so the rail
        // settled on whichever landed last while the pane header - which reads
        // the live title - showed the other name.
        thread.title_from_process = true;
        assert!(may_name_surface(&thread, TitleSource::Process));
        assert!(!may_name_surface(&thread, TitleSource::StoredSummary));

        // And the flag does not survive a restart, so a restored row is named
        // by its summary again until its CLI paints.
        let restored = Thread::new_terminal("session", "/repo", None);
        assert!(!restored.title_from_process);
        assert!(may_name_surface(&restored, TitleSource::StoredSummary));
    }

    #[test]
    fn a_manual_rename_outranks_both_automatic_sources() {
        use crate::project::Thread;
        let mut thread = Thread::new_terminal("session", "/repo", None);
        thread.title_user_set = true;
        assert!(!may_name_surface(&thread, TitleSource::Process));
        assert!(!may_name_surface(&thread, TitleSource::StoredSummary));
    }

    fn session_meta(id: &str, summary: &str) -> crate::agent_sessions::SessionMeta {
        crate::agent_sessions::SessionMeta {
            agent: crate::agent_sessions::SessionAgent::Claude,
            session_id: id.to_string(),
            last_activity_secs: 0,
            timestamp: "2026-07-05T12:00:00Z".to_string(),
            cwd: "/repo".to_string(),
            git_branch: "main".to_string(),
            summary: Some(summary.to_string()),
            first_prompt: None,
            model: None,
            usage: None,
        }
    }
}
