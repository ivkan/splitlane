//! Agent-sessions right sidebar.
//!
//! Docked panel that replaces the former anchored popover: it lists the active
//! terminal's cwd-scoped sessions for every enabled agent with a documented
//! local list+resume contract as stacked groups. Toggled by the tab-bar sessions button via
//! `PaneEvent::ToggleAgentSessions`; it stays open while you work because it is
//! a layout child of the root row, not a `deferred()` overlay. Clicking a row
//! issues the agent's `--resume` command into the bound pane and keeps the
//! sidebar open.
//!
//! Reuses the session data layer verbatim (`SessionMeta`,
//! `read_sessions_for_cwd`, `enabled_session_agents`). Per-group cap-5 /
//! "Show more" / collapse caret and the per-group "new session" affordance were
//! layered on top of that; the surface swap itself rendered flat groups.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Pixels, SharedString, Styled, Window, div, img, prelude::*, px,
    rgb, svg,
};

use crate::SplitlaneApp;
use crate::agent_launcher::AgentCommandSpec;
use crate::agent_sessions::{SessionAgent, SessionMeta};
use crate::app::ipc_handler::find_pane_by_surface_id;
use crate::pane_drag::{SessionDrag, TabDragPreview};
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// Fixed sidebar width - between the CLI (220) and Agents (280) left sidebars,
/// matching VS Code's secondary-bar default. Resizable width is deferred.
pub(crate) const SESSIONS_SIDEBAR_WIDTH: f32 = 300.;
const ROW_HEIGHT: Pixels = crate::app::constants::LIST_ROW_HEIGHT;

impl SplitlaneApp {
    /// Open (or re-target) the sessions sidebar for `pane`: resolve the
    /// pane's terminal cwd, bind the resume target, reset per-group state,
    /// and kick the per-agent scans. Shared by the tab-bar toggle
    /// (`PaneEvent::ToggleAgentSessions`) and the workspace switch
    /// (`select_workspace` re-targets an open sidebar to the new active
    /// workspace through this same path).
    pub(crate) fn open_sessions_sidebar_for_pane(
        &mut self,
        pane: &gpui::Entity<crate::pane::Pane>,
        focus_window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        // Resolve the active terminal's cwd: prefer the OSC 7 push
        // (`current_cwd`), fall back to the on-demand `cwd_now()` syscall for
        // shells that don't emit OSC 7.
        let terminal = pane.read(cx).active_terminal_opt();
        let surface_id = terminal.as_ref().map(|tv| tv.entity_id().as_u64());
        let cwd_str = terminal.as_ref().and_then(|tv| {
            let view = tv.read(cx);
            view.terminal.current_cwd.clone().or_else(|| {
                view.terminal
                    .cwd_now()
                    .map(|p| p.to_string_lossy().into_owned())
            })
        });

        // Mutual exclusion: only one right column. Opening sessions closes
        // the Files sidebar (and vice-versa, in `toggle_files_sidebar`).
        if self.files_sidebar_open {
            self.close_files_sidebar(cx);
        }

        // Close floating dropdowns so they don't paint over the newly opened
        // docked sidebar.
        self.dismiss_transient_surfaces();

        self.set_sessions_sidebar_open(true, cx);
        self.agent_sessions.sessions_cwd = cwd_str.clone();
        self.agent_sessions.sessions_surface_id = surface_id;
        // Release any project the Agents-mode path had bound, so a sidebar
        // re-opened from a pane resumes into that pane and not into a thread.
        self.agent_sessions.sessions_project_idx = None;
        for sessions in &mut self.agent_sessions.sessions_by_agent {
            sessions.clear();
        }
        // Fresh per-group state for this open: all expanded, capped at 5,
        // not-yet-scanning (each spawned scan flips its own flag below).
        self.agent_sessions.sessions_omitted = [0; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_group_collapsed =
            [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_group_show_all =
            [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_scanning = [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_selected = 0;
        self.agent_sessions.sessions_scan_generation =
            self.agent_sessions.sessions_scan_generation.wrapping_add(1);
        let scan_generation = self.agent_sessions.sessions_scan_generation;
        let enabled_agents =
            crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config);
        // Fresh handle so a previous scroll offset doesn't bleed into the new
        // sidebar.
        self.agent_sessions.sessions_scroll = gpui::ScrollHandle::new();

        if let Some(window) = focus_window {
            self.agent_sessions.sessions_focus.focus(window, cx);
        }

        if let Some(cwd) = cwd_str {
            // Parallel scans. Each supported agent owns a documented native
            // contract (JSONL store or CLI list command) and writes to its own
            // Vec on the main thread. The sidebar may be closed or re-targeted
            // against a different cwd before any scan finishes, so stale
            // results are dropped by checking the target cwd and scan
            // generation before applying.
            //
            // Scans for agents the user has hidden in Settings → AI Agent are
            // skipped: with no UI to surface them the disk read would just be
            // wasted I/O.
            for agent in enabled_agents {
                self.spawn_sessions_scan(agent, cwd.clone(), scan_generation, cx);
            }
        }
        cx.notify();
    }

    /// Open the sessions sidebar for a project in Agents mode.
    ///
    /// Same scan machinery as [`Self::open_sessions_sidebar_for_pane`], with
    /// the two differences that Agents mode forces: the cwd comes from the
    /// project rather than from a live terminal's OSC 7 (there need not be any
    /// terminal open yet), and picking a row opens the session as a *thread*
    /// instead of injecting a resume command into a bound pane. That is what
    /// makes history usable here at all - Agents mode has threads, not panes,
    /// so the pane-bound path had nothing to target.
    pub(crate) fn open_sessions_sidebar_for_project(
        &mut self,
        ws_idx: usize,
        focus_window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(cwd) = self.workspaces.get(ws_idx).map(|p| p.cwd.clone()) else {
            return;
        };

        if self.files_sidebar_open {
            self.close_files_sidebar(cx);
        }
        self.dismiss_transient_surfaces();

        self.set_sessions_sidebar_open(true, cx);
        self.agent_sessions.sessions_cwd = Some(cwd.clone());
        // Claim the resume target for the project and release any pane the
        // CLI-mode path had bound: the two are mutually exclusive.
        self.agent_sessions.sessions_project_idx = Some(ws_idx);
        self.agent_sessions.sessions_surface_id = None;
        for sessions in &mut self.agent_sessions.sessions_by_agent {
            sessions.clear();
        }
        self.agent_sessions.sessions_omitted = [0; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_group_collapsed =
            [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_group_show_all =
            [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_scanning = [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_selected = 0;
        self.agent_sessions.sessions_scan_generation =
            self.agent_sessions.sessions_scan_generation.wrapping_add(1);
        let scan_generation = self.agent_sessions.sessions_scan_generation;
        let enabled_agents =
            crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config);
        self.agent_sessions.sessions_scroll = gpui::ScrollHandle::new();

        if let Some(window) = focus_window {
            self.agent_sessions.sessions_focus.focus(window, cx);
        }

        for agent in enabled_agents {
            self.spawn_sessions_scan(agent, cwd.clone(), scan_generation, cx);
        }
        cx.notify();
    }

    fn spawn_sessions_scan(
        &mut self,
        agent: SessionAgent,
        cwd: String,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let idx = agent_index(agent);
        self.agent_sessions.sessions_scanning[idx] = true;
        cx.spawn(async move |this, cx| {
            let scan_cwd = cwd.clone();
            let started = std::time::Instant::now();
            let (sessions, omitted) = smol::unblock(move || {
                crate::agent_sessions::read_sessions_for_cwd_with_omitted(agent, &scan_cwd)
            })
            .await;
            let elapsed = started.elapsed();
            let retained = sessions.len();
            log::debug!(
                "agent sessions scan {:?} cwd={} retained={} omitted={} elapsed={:?}",
                agent,
                cwd,
                retained,
                omitted,
                elapsed
            );
            let _ = this.update(cx, |app, cx| {
                if should_apply_scan_result(
                    app.agent_sessions.sessions_sidebar_open,
                    app.agent_sessions.sessions_cwd.as_deref(),
                    &cwd,
                    app.agent_sessions.sessions_scan_generation,
                    generation,
                ) {
                    *app.sessions_for_mut(agent) = sessions;
                    app.agent_sessions.sessions_omitted[idx] = omitted;
                    app.agent_sessions.sessions_scanning[idx] = false;
                    app.clamp_sessions_selection();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Render the docked sessions sidebar (right edge of the root `flex_row`).
    /// Only called while the sidebar is open or animating closed.
    pub(crate) fn render_sessions_sidebar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = crate::theme::ui_colors();
        div()
            .id("sessions-sidebar")
            .flex()
            .flex_col()
            .w(px(SESSIONS_SIDEBAR_WIDTH))
            .flex_shrink_0()
            .h_full()
            .track_focus(&self.agent_sessions.sessions_focus)
            .on_key_down(cx.listener(Self::handle_sessions_sidebar_key_down))
            // Match the app's other navigation rails: optional native material
            // on Windows, platform default on macOS, and a light/dark tint on Linux.
            .bg(crate::app::constants::cockpit_chrome_background(
                ui.chrome_for(window.is_window_active()),
                self.cached_config.cockpit_chrome_material_enabled(),
            ))
            .child(self.sessions_sidebar_header(ui, cx))
            .child(self.sessions_sidebar_filter(ui, cx))
            .child(self.sessions_sidebar_body(ui, cx))
            .into_any_element()
    }

    /// Search field filtering the rows by title. Mirrors the Agents sidebar
    /// field down to the shared [`crate::ui_primitives::filter_pill`] recipe,
    /// so both rails read as one system.
    fn sessions_sidebar_filter(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_empty = self
            .agent_sessions
            .sessions_filter_input
            .read(cx)
            .value()
            .is_empty();
        div()
            .flex_none()
            .px(tok::space::XL)
            // The header above is a fixed 34px carrying two lines - the panel
            // name and the project - so its second line ends hard against
            // whatever follows. Without this the search field started 4px
            // under the project name, in the same weight and nearly the same
            // colour, and the two read as one block. Reported from real use.
            // Space rather than a divider, which is what the header's own
            // note asks for.
            .pt(tok::space::MD)
            .pb(tok::space::XS)
            .child(
                crate::ui_primitives::filter_pill(
                    "sessions-sidebar-filter",
                    "sessions-sidebar-filter-clear",
                    ui,
                    self.agent_sessions.sessions_filter_input.clone(),
                    !is_empty,
                    cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.agent_sessions
                            .sessions_filter_input
                            .update(cx, |inp, cx| inp.clear(cx));
                    }),
                )
                // Escape clears the query rather than closing the sidebar: the
                // field swallows it so a search can be abandoned without losing
                // the panel. Everything else (caret, selection, clipboard) is
                // handled inside the focused TextInput.
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                    if ev.keystroke.key.as_str() == "escape" {
                        this.agent_sessions
                            .sessions_filter_input
                            .update(cx, |inp, cx| inp.clear(cx));
                        cx.stop_propagation();
                    }
                })),
            )
            .into_any_element()
    }

    fn sessions_sidebar_header(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover_background = crate::app::constants::sidebar_tab_hover_background();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(tok::space::MD)
            // Quiet header - no divider (Codex: separation by spacing, not
            // borders), at the design's panel-header height. It carries a
            // second line - the cwd - inside that height rather than growing.
            .h(tok::row::HEADER)
            .flex_none()
            .px(tok::space::XL)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(tok::space::XS)
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .overflow_x_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(tok::text::ROW)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui.text)
                            .child("Agent sessions"),
                    )
                    .when_some(self.agent_sessions.sessions_cwd.as_deref(), |d, cwd| {
                        d.child(
                            div()
                                .id("sessions-sidebar-cwd")
                                .overflow_x_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(tok::text::CAPTION)
                                // One step above `muted`, which is what the
                                // search field's placeholder is painted in:
                                // the two sat four pixels apart in the same
                                // colour and the same weight, so the project
                                // name read as part of the search box. Lifting
                                // the fact rather than dimming the placeholder
                                // - a placeholder is not content, but making
                                // every input in the app quieter to fix one
                                // header is not a trade this needs.
                                .text_color(ui.text_tertiary)
                                .tooltip(crate::ui_primitives::text_tooltip(cwd.to_string()))
                                .child(compact_cwd_label(cwd)),
                        )
                    }),
            )
            .child(
                div()
                    .id("sessions-sidebar-close")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(22.))
                    .rounded(tok::radius::TAG)
                    .text_size(tok::text::BODY)
                    .text_color(ui.muted)
                    .animated_hover(move |style, delta| {
                        style
                            .bg(lerp_color(
                                hover_background.opacity(0.0),
                                hover_background,
                                delta,
                            ))
                            .text_color(lerp_color(ui.muted, ui.text, delta));
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.close_sessions_sidebar(cx);
                        cx.stop_propagation();
                    }))
                    .child("×"),
            )
            .into_any_element()
    }

    fn sessions_sidebar_body(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.agent_sessions.sessions_cwd.is_none() {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .p(tok::space::XL)
                .child(
                    div()
                        .text_size(tok::text::ROW)
                        .text_color(ui.muted)
                        .child("Could not detect the terminal's working directory."),
                )
                .into_any_element();
        }

        // An agent can be toggled off in Settings while the sidebar is
        // open. The list is driven by the cached config snapshot, so a disabled
        // agent's group disappears on the next render after propagation; if the
        // user disables them all, show an empty state rather than a blank panel.
        let enabled =
            crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config);
        if enabled.is_empty() {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .p(tok::space::XL)
                .child(
                    div()
                        .text_size(tok::text::ROW)
                        .text_color(ui.muted)
                        .child("No AI agents enabled. Enable one in Settings → AI Agent."),
                )
                .into_any_element();
        }

        // The groups go into a column that does not shrink, and that column is
        // the scroller's only child: a scroller's items have no automatic
        // minimum height, so fixed-height session rows gave up height to fit a
        // long history instead of overflowing and scrolling.
        let mut body = div().flex_none().w_full().flex().flex_col();

        let selected = self.selected_session_target();
        let mut groups_rendered = 0usize;
        let mut scanning_any = false;
        for agent in enabled {
            let idx = agent_index(agent);
            let scanning = self.agent_sessions.sessions_scanning[idx];
            scanning_any |= scanning;
            if scanning || !self.sessions_visible_for(agent).is_empty() {
                groups_rendered += 1;
                body = body.child(self.sessions_group(agent, ui, selected, cx));
            }
        }
        if groups_rendered == 0 {
            let message = if scanning_any {
                "Scanning sessions..."
            } else if self.sessions_query_active() {
                "No sessions match this search."
            } else {
                "No sessions for this directory yet."
            };
            body = body.child(
                div()
                    .p(tok::space::XL)
                    .text_size(tok::text::ROW)
                    .text_color(ui.muted)
                    .child(message),
            );
        }
        div()
            .id("sessions-sidebar-body")
            .flex()
            .flex_col()
            .flex_1()
            .py(tok::space::XS)
            // Vertical scroll only - never let a long row title push the
            // panel into horizontal scrolling.
            .overflow_x_hidden()
            .overflow_y_scroll()
            .track_scroll(&self.agent_sessions.sessions_scroll)
            .child(body)
            .into_any_element()
    }

    fn sessions_group(
        &self,
        agent: SessionAgent,
        ui: crate::theme::UiColors,
        selected: Option<SessionNavTarget<'_>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let idx = agent_index(agent);
        let collapsed = self.sessions_group_collapsed_effective(idx);
        let show_all = self.sessions_group_show_all_effective(idx);
        let scanning = self.agent_sessions.sessions_scanning[idx];
        let sessions = self.sessions_visible_for(agent);
        let omitted = self.sessions_omitted_for(agent);
        // Distinct chevron per state: right = collapsed, down =
        // expanded - a static swap, not a tween, so it reads under reduced
        // motion.
        let chevron = if collapsed {
            "icons/chevron-right.svg"
        } else {
            "icons/chevron-down.svg"
        };

        // The whole header toggles the group's collapse. Styled as a
        // section eyebrow (the Agents-sidebar language): small semibold muted
        // label, brand glyph kept in its native accent - the only color in
        // the rail, carrying real signal (which tool).
        let header = div()
            .id(SharedString::from(format!(
                "sessions-group-{}",
                agent_id_prefix(agent)
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .px(tok::space::XL)
            .pt(tok::space::XL)
            .pb(tok::space::XS)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.agent_sessions.sessions_focus.focus(window, cx);
                this.agent_sessions.sessions_group_collapsed[idx] =
                    !this.agent_sessions.sessions_group_collapsed[idx];
                this.clamp_sessions_selection();
                cx.notify();
            }))
            .child(agent_icon_element(agent, px(14.), ui))
            .child(
                div()
                    .text_size(tok::text::CAPTION)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui.muted)
                    .child(agent_label(agent)),
            )
            // Collapse chevron, sitting just after the agent name.
            .child(
                svg()
                    .size(px(12.))
                    .flex_none()
                    .path(chevron)
                    .text_color(ui.muted),
            );

        let mut group = div().flex().flex_col().child(header);

        // Collapsed → header only.
        if collapsed {
            return group.into_any_element();
        }

        if sessions.is_empty() {
            // Distinguish a pending scan from a genuinely empty group.
            let msg: SharedString = if scanning {
                SharedString::from("Scanning\u{2026}")
            } else {
                empty_message(agent)
            };
            group = group.child(
                div()
                    .mx(tok::space::XL)
                    .px(tok::space::MD)
                    .py(tok::space::XS)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted.opacity(0.8))
                    .child(msg),
            );
        } else {
            // Cap at 5, reveal the rest behind "Show N more".
            let (visible, remaining) = visible_window(sessions.len(), show_all, CAP);
            for session in sessions.iter().take(visible) {
                group = group.child(self.sessions_row(
                    session,
                    ui,
                    selected.is_some_and(|target| {
                        target.agent == agent && target.session_id == session.session_id
                    }),
                    cx,
                ));
            }
            if sessions.len() > CAP {
                let hover_background = crate::app::constants::sidebar_tab_hover_background();
                let label: SharedString = if show_all {
                    SharedString::from("Show less")
                } else {
                    format!("Show {remaining} more").into()
                };
                group = group.child(
                    div()
                        .id(SharedString::from(format!(
                            "{}-show-more",
                            agent_id_prefix(agent)
                        )))
                        .mx(tok::space::XS)
                        .px(tok::space::MD)
                        .py(tok::space::XS)
                        .rounded(tok::radius::SMALL)
                        .text_size(tok::text::CAPTION)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui.muted)
                        .animated_hover(move |style, delta| {
                            style
                                .bg(lerp_color(
                                    hover_background.opacity(0.0),
                                    hover_background,
                                    delta,
                                ))
                                .text_color(lerp_color(ui.muted, ui.text, delta));
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.agent_sessions.sessions_focus.focus(window, cx);
                            this.agent_sessions.sessions_group_show_all[idx] =
                                !this.agent_sessions.sessions_group_show_all[idx];
                            this.clamp_sessions_selection();
                            cx.notify();
                        }))
                        .child(label),
                );
            }
            if omitted > 0 {
                group = group.child(
                    div()
                        .mx(tok::space::XL)
                        .px(tok::space::MD)
                        .py(tok::space::XS)
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.muted.opacity(0.8))
                        .child(older_sessions_hidden_label(omitted)),
                );
            }
        }

        group.into_any_element()
    }

    fn sessions_for(&self, agent: SessionAgent) -> &[SessionMeta] {
        &self.agent_sessions.sessions_by_agent[agent_index(agent)]
    }

    /// The rows the sidebar actually shows for `agent` under the current
    /// query. Every render AND navigation path goes through this: if the two
    /// disagreed, the keyboard selection would address a row the user cannot
    /// see.
    fn sessions_visible_for(&self, agent: SessionAgent) -> Vec<&SessionMeta> {
        let needle = self.agent_sessions.sessions_query.as_str();
        self.sessions_for(agent)
            .iter()
            .filter(|session| session_matches(session, needle))
            .collect()
    }

    fn sessions_query_active(&self) -> bool {
        !self.agent_sessions.sessions_query.is_empty()
    }

    /// A search has to beat both of the affordances that hide rows, or the
    /// match the user typed for stays invisible: a collapsed group renders no
    /// rows at all, and the 5-row cap parks the rest behind "Show more". While
    /// a query is active both are overridden; clearing it restores whatever the
    /// user had set.
    fn sessions_group_collapsed_effective(&self, idx: usize) -> bool {
        self.agent_sessions.sessions_group_collapsed[idx] && !self.sessions_query_active()
    }

    fn sessions_group_show_all_effective(&self, idx: usize) -> bool {
        self.agent_sessions.sessions_group_show_all[idx] || self.sessions_query_active()
    }

    fn sessions_for_mut(&mut self, agent: SessionAgent) -> &mut Vec<SessionMeta> {
        &mut self.agent_sessions.sessions_by_agent[agent_index(agent)]
    }

    fn sessions_omitted_for(&self, agent: SessionAgent) -> usize {
        self.agent_sessions.sessions_omitted[agent_index(agent)]
    }

    fn sessions_row(
        &self,
        session: &SessionMeta,
        ui: crate::theme::UiColors,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let agent = session.agent;
        let session_id = session.session_id.clone();
        let row_id = SharedString::from(format!("{}-session-{session_id}", agent_id_prefix(agent)));
        let hover_background = crate::app::constants::sidebar_tab_hover_background();
        let resting_background = if selected {
            hover_background
        } else {
            hover_background.opacity(0.0)
        };
        // Last activity, not creation time - the label has to agree with the
        // sort order, or the top row reads as older than the one below it.
        let when = SharedString::from(crate::agent_sessions::format_session_recency(session));
        let title: SharedString = session
            .summary
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| short_session_id(&session_id))
            .into();

        // Drag payload: dropping this row on a pane spawns a fresh terminal at
        // the session's cwd running its resume command (drop-to-split / append
        // as a tab). The ghost reuses the tab-drag preview.
        let drag_payload = SessionDrag {
            agent,
            session_id: session_id.clone(),
            cwd: session.cwd.clone(),
            title: title.clone(),
            icon: SharedString::from(agent_icon_path(agent)),
        };

        div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .h(ROW_HEIGHT)
            .mx(tok::space::MD)
            .my(px(1.))
            .px(tok::space::MD)
            .rounded(tok::radius::SMALL)
            .on_drag(drag_payload, |drag, _offset, _window, cx| {
                cx.new(|_| TabDragPreview {
                    title: drag.title.clone(),
                    icon: drag.icon.clone(),
                })
            })
            .bg(resting_background)
            .animated_hover(move |style, delta| {
                style.bg(lerp_color(resting_background, hover_background, delta));
            })
            .cursor_pointer()
            // Resume into the bound pane; the docked sidebar
            // stays open (unlike the old popover).
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.agent_sessions.sessions_focus.focus(window, cx);
                this.select_session_row(agent, &session_id);
                this.resume_session_from_sidebar(agent, &session_id, cx);
                cx.stop_propagation();
            }))
            // Per-session agent glyph in its brand accent - a touch smaller
            // than the group-header mark so the header still reads as the
            // section anchor.
            .child(agent_icon_element(agent, px(13.), ui))
            // Title takes the slack and ellipsizes; the relative time is pinned
            // to the trailing edge on the same line (the row stays one line).
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(tok::text::ROW)
                    .text_color(ui.text)
                    .overflow_x_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted)
                    .child(when),
            )
            .into_any_element()
    }

    fn resume_session_from_sidebar(
        &mut self,
        agent: SessionAgent,
        session_id: &str,
        cx: &mut Context<Self>,
    ) {
        // Agents mode: the sidebar is bound to a project, so the session
        // becomes a thread of its own rather than a command typed into
        // somebody else's pane. `SessionBinding::resolve` sees the adopted id
        // already has a transcript and resumes it on the thread's first mount.
        if let Some(ws_idx) = self.agent_sessions.sessions_project_idx {
            let terminal_agent = agent.terminal_agent();
            let title = self
                .agent_sessions
                .sessions_by_agent
                .get(agent.index())
                .and_then(|rows| rows.iter().find(|s| s.session_id == session_id))
                .and_then(|s| s.summary.clone())
                .unwrap_or_else(|| terminal_agent.display_name().to_string());
            self.open_session_as_thread_in(ws_idx, terminal_agent, session_id, title, cx);
            return;
        }
        let Some(command) = resume_command(agent, session_id, &self.cached_config) else {
            self.show_toast("Could not resume session - invalid session id", cx);
            return;
        };
        match self.send_command_to_sessions_surface(&command, cx) {
            ResumeSendResult::Sent => {}
            ResumeSendResult::Missing => {
                self.show_toast("Could not resume session - target terminal is gone", cx);
            }
            ResumeSendResult::WrongCwd => {
                self.show_toast(
                    "Could not resume session - target terminal changed directory",
                    cx,
                );
            }
        }
    }

    fn send_command_to_sessions_surface(
        &self,
        command: &str,
        cx: &mut Context<Self>,
    ) -> ResumeSendResult {
        let Some(surface_id) = self.agent_sessions.sessions_surface_id else {
            return ResumeSendResult::Missing;
        };
        let Some(expected_cwd) = self.agent_sessions.sessions_cwd.as_deref() else {
            return ResumeSendResult::Missing;
        };
        let Some((_ws_idx, pane, tab_idx)) =
            find_pane_by_surface_id(&self.workspaces, surface_id, cx)
        else {
            return ResumeSendResult::Missing;
        };
        let Some(terminal) = pane
            .read(cx)
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.as_terminal())
            .cloned()
        else {
            return ResumeSendResult::Missing;
        };
        let current_cwd = {
            let view = terminal.read(cx);
            view.terminal.current_cwd.clone().or_else(|| {
                view.terminal
                    .cwd_now()
                    .map(|p| p.to_string_lossy().into_owned())
            })
        };
        if !current_cwd
            .as_deref()
            .is_some_and(|cwd| crate::agent_sessions::cwd_matches(cwd, expected_cwd))
        {
            return ResumeSendResult::WrongCwd;
        }
        terminal.read(cx).send_command(command);
        ResumeSendResult::Sent
    }

    fn handle_sessions_sidebar_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.sessions_nav_len();
        match event.keystroke.key.as_str() {
            "escape" => self.close_sessions_sidebar(cx),
            "enter" | "space" if len > 0 => {
                let selected = self.agent_sessions.sessions_selected.min(len - 1);
                if let Some(target) = self.sessions_nav_target_at(selected) {
                    let agent = target.agent;
                    let session_id = target.session_id.to_string();
                    self.resume_session_from_sidebar(agent, &session_id, cx);
                }
            }
            "up" if len > 0 => {
                self.agent_sessions.sessions_selected = moved_session_selection(
                    self.agent_sessions.sessions_selected,
                    len,
                    SessionSelectionMove::Previous,
                );
                cx.notify();
            }
            "down" if len > 0 => {
                self.agent_sessions.sessions_selected = moved_session_selection(
                    self.agent_sessions.sessions_selected,
                    len,
                    SessionSelectionMove::Next,
                );
                cx.notify();
            }
            "home" if len > 0 => {
                self.agent_sessions.sessions_selected = moved_session_selection(
                    self.agent_sessions.sessions_selected,
                    len,
                    SessionSelectionMove::First,
                );
                cx.notify();
            }
            "end" if len > 0 => {
                self.agent_sessions.sessions_selected = moved_session_selection(
                    self.agent_sessions.sessions_selected,
                    len,
                    SessionSelectionMove::Last,
                );
                cx.notify();
            }
            _ => {}
        }
    }

    fn sessions_nav_len(&self) -> usize {
        let mut len = 0;
        for agent in crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config)
        {
            let idx = agent_index(agent);
            if self.sessions_group_collapsed_effective(idx) {
                continue;
            }
            let sessions = self.sessions_visible_for(agent);
            let (visible, _) = visible_window(
                sessions.len(),
                self.sessions_group_show_all_effective(idx),
                CAP,
            );
            len += visible;
        }
        len
    }

    fn sessions_nav_target_at(&self, index: usize) -> Option<SessionNavTarget<'_>> {
        let mut cursor = 0usize;
        for agent in crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config)
        {
            let idx = agent_index(agent);
            if self.sessions_group_collapsed_effective(idx) {
                continue;
            }
            let sessions = self.sessions_visible_for(agent);
            let (visible, _) = visible_window(
                sessions.len(),
                self.sessions_group_show_all_effective(idx),
                CAP,
            );
            if index < cursor + visible {
                let session = &sessions[index - cursor];
                return Some(SessionNavTarget {
                    agent,
                    session_id: &session.session_id,
                });
            }
            cursor += visible;
        }
        None
    }

    fn selected_session_target(&self) -> Option<SessionNavTarget<'_>> {
        let len = self.sessions_nav_len();
        if len == 0 {
            return None;
        }
        self.sessions_nav_target_at(self.agent_sessions.sessions_selected.min(len - 1))
    }

    fn select_session_row(&mut self, agent: SessionAgent, session_id: &str) {
        let mut cursor = 0usize;
        for row_agent in
            crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config)
        {
            let idx = agent_index(row_agent);
            if self.sessions_group_collapsed_effective(idx) {
                continue;
            }
            let sessions = self.sessions_visible_for(row_agent);
            let (visible, _) = visible_window(
                sessions.len(),
                self.sessions_group_show_all_effective(idx),
                CAP,
            );
            for session in sessions.iter().take(visible) {
                if row_agent == agent && session.session_id == session_id {
                    self.agent_sessions.sessions_selected = cursor;
                    return;
                }
                cursor += 1;
            }
        }
    }

    pub(crate) fn clamp_sessions_selection(&mut self) {
        let len = self.sessions_nav_len();
        if len == 0 {
            self.agent_sessions.sessions_selected = 0;
        } else if self.agent_sessions.sessions_selected >= len {
            self.agent_sessions.sessions_selected = len - 1;
        }
    }

    fn sessions_sidebar_width_at(&self, now: std::time::Instant) -> f32 {
        if let Some(animation) = self.agent_sessions.sessions_sidebar_animation {
            animation.width_at(now)
        } else if self.agent_sessions.sessions_sidebar_open {
            SESSIONS_SIDEBAR_WIDTH
        } else {
            0.
        }
    }

    pub(crate) fn rendered_sessions_sidebar_width(&mut self, window: &mut Window) -> f32 {
        let now = std::time::Instant::now();
        if let Some(animation) = self.agent_sessions.sessions_sidebar_animation {
            if animation.is_finished(now) {
                self.agent_sessions.sessions_sidebar_animation = None;
                if !self.agent_sessions.sessions_sidebar_open {
                    self.clear_sessions_sidebar_state();
                }
                animation.to_width
            } else {
                window.request_animation_frame();
                animation.width_at(now)
            }
        } else if self.agent_sessions.sessions_sidebar_open {
            SESSIONS_SIDEBAR_WIDTH
        } else {
            0.
        }
    }

    fn set_sessions_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let from_width = self.sessions_sidebar_width_at(now);
        self.agent_sessions.sessions_sidebar_open = open;
        let to_width = if open { SESSIONS_SIDEBAR_WIDTH } else { 0. };

        self.agent_sessions.sessions_sidebar_animation =
            if (from_width - to_width).abs() > crate::PRIMARY_SIDEBAR_MIN_ANIMATION_DELTA {
                Some(crate::SidebarWidthAnimation {
                    from_width,
                    to_width,
                    started_at: now,
                })
            } else {
                None
            };

        if !open && self.agent_sessions.sessions_sidebar_animation.is_none() {
            self.clear_sessions_sidebar_state();
        }
        cx.notify();
    }

    fn clear_sessions_sidebar_state(&mut self) {
        for sessions in &mut self.agent_sessions.sessions_by_agent {
            sessions.clear();
        }
        self.agent_sessions.sessions_omitted = [0; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_cwd = None;
        self.agent_sessions.sessions_surface_id = None;
        self.agent_sessions.sessions_project_idx = None;
        self.agent_sessions.sessions_selected = 0;
        self.agent_sessions.sessions_group_collapsed =
            [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_group_show_all =
            [false; crate::agent_sessions::SESSION_AGENT_COUNT];
        self.agent_sessions.sessions_scanning = [false; crate::agent_sessions::SESSION_AGENT_COUNT];
    }

    pub(crate) fn close_sessions_sidebar_immediate(&mut self, cx: &mut Context<Self>) {
        self.agent_sessions.sessions_sidebar_open = false;
        self.agent_sessions.sessions_sidebar_animation = None;
        self.agent_sessions.sessions_scan_generation =
            self.agent_sessions.sessions_scan_generation.wrapping_add(1);
        self.clear_sessions_sidebar_state();
        cx.notify();
    }

    /// Start closing the sidebar and invalidate in-flight scans immediately.
    /// The visible rows are cleared only after the width animation reaches
    /// zero, so the closing panel never flashes an empty-state body.
    pub(crate) fn close_sessions_sidebar(&mut self, cx: &mut Context<Self>) {
        self.agent_sessions.sessions_scan_generation =
            self.agent_sessions.sessions_scan_generation.wrapping_add(1);
        self.set_sessions_sidebar_open(false, cx);
    }
}

/// Default per-group row cap before "Show more".
const CAP: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionNavTarget<'a> {
    agent: SessionAgent,
    session_id: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionSelectionMove {
    First,
    Last,
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeSendResult {
    Sent,
    Missing,
    WrongCwd,
}

fn moved_session_selection(current: usize, len: usize, movement: SessionSelectionMove) -> usize {
    if len == 0 {
        return 0;
    }
    match movement {
        SessionSelectionMove::First => 0,
        SessionSelectionMove::Last => len - 1,
        SessionSelectionMove::Previous => current.saturating_sub(1),
        SessionSelectionMove::Next => (current + 1).min(len - 1),
    }
}

fn should_apply_scan_result(
    sidebar_open: bool,
    current_cwd: Option<&str>,
    expected_cwd: &str,
    current_generation: u64,
    expected_generation: u64,
) -> bool {
    sidebar_open && current_cwd == Some(expected_cwd) && current_generation == expected_generation
}

/// Stable group index for the per-agent state arrays. Shared with
/// `event_handlers` so the scan-in-flight flag and the render read the same
/// slot.
pub(crate) fn agent_index(agent: SessionAgent) -> usize {
    agent.index()
}

/// Given a group of `len` rows, the cap, and whether the group is expanded,
/// return `(visible, remaining)`: how many rows to render and how many are
/// hidden behind "Show more". Pure - unit-tested.
/// Does this row survive the sidebar's search query? `lowered_needle` must
/// already be lowercased (the caller keeps one lowered copy in state).
///
/// Matches what the row actually displays: the session title, falling back to
/// the short id for sessions that never got one. Transcript bodies are
/// deliberately out of scope - searching those means reading every JSONL off
/// disk on each keystroke.
fn session_matches(session: &SessionMeta, lowered_needle: &str) -> bool {
    if lowered_needle.is_empty() {
        return true;
    }
    let title = session.summary.as_deref().unwrap_or("");
    crate::app::agents_sidebar::filter::matches(title, lowered_needle)
        || crate::app::agents_sidebar::filter::matches(&session.session_id, lowered_needle)
        // And the words the person actually typed. The title above is the
        // agent's, written once near the start, so a session whose subject has
        // moved on is findable by neither its current name nor its first one.
        || session
            .first_prompt
            .as_deref()
            .is_some_and(|prompt| crate::app::agents_sidebar::filter::matches(prompt, lowered_needle))
}

fn visible_window(len: usize, show_all: bool, cap: usize) -> (usize, usize) {
    if show_all || len <= cap {
        (len, 0)
    } else {
        (cap, len - cap)
    }
}

fn older_sessions_hidden_label(omitted: usize) -> SharedString {
    if omitted == 1 {
        SharedString::from("1 older session hidden")
    } else {
        format!("{omitted} older sessions hidden").into()
    }
}

fn compact_cwd_label(cwd: &str) -> SharedString {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    let label = trimmed
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(trimmed);
    if label.is_empty() {
        cwd.into()
    } else {
        label.into()
    }
}

fn empty_message(agent: SessionAgent) -> SharedString {
    format!("No {} sessions for this directory yet.", agent_label(agent)).into()
}

fn short_session_id(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

fn agent_id_prefix(agent: SessionAgent) -> &'static str {
    agent.terminal_agent().tag()
}

/// Display name for a group header.
fn agent_label(agent: SessionAgent) -> &'static str {
    agent.label()
}

/// Brand glyph for a group/session - the same monochrome (`currentColor`) SVGs
/// the tab-bar launcher buttons use, tinted at the call site.
fn agent_icon_path(agent: SessionAgent) -> &'static str {
    agent.icon_path()
}

/// Accent for a group's brand glyph - matches the launcher buttons in
/// `pane.rs` (Claude orange, Codex blue). OpenCode's mark is monochrome, so it
/// rides the theme text color to stay legible on dark and light surfaces.
fn agent_brand_color(agent: SessionAgent, ui: crate::theme::UiColors) -> Hsla {
    agent
        .terminal_agent()
        .accent()
        .map(|accent| rgb(accent).into())
        .unwrap_or(ui.text)
}

fn agent_icon_element(agent: SessionAgent, size: Pixels, ui: crate::theme::UiColors) -> AnyElement {
    if agent.terminal_agent().icon_multicolor() {
        img(agent_icon_path(agent))
            .size(size)
            .flex_none()
            .into_any_element()
    } else {
        svg()
            .size(size)
            .flex_none()
            .path(agent_icon_path(agent))
            .text_color(agent_brand_color(agent, ui))
            .into_any_element()
    }
}

/// True when Settings -> AI Agent has `claude_code_bypass_permissions` toggled
/// on in the caller's config snapshot.
fn claude_bypass_enabled(config: &splitlane_config::schema::SplitlaneConfig) -> bool {
    config.claude_code_bypass_permissions.unwrap_or(false)
}

/// Build the command sent to the bound terminal when a session row is clicked.
/// For Claude, honor `claude_code_bypass_permissions` so resumed sessions match
/// a fresh launch from the tab-bar button.
///
/// Returns `None` when `session_id` fails the strict allow-list - a last gate
/// before interpolation so a tampered record that somehow bypassed the scanner
/// filter (`*_sessions.rs`) can never inject a second shell command. Callers
/// skip the send on `None`.
pub(crate) fn resume_command(
    agent: SessionAgent,
    session_id: &str,
    config: &splitlane_config::schema::SplitlaneConfig,
) -> Option<String> {
    resume_command_spec(agent, session_id, config).map(|spec| spec.render_shell_command())
}

fn resume_command_spec(
    agent: SessionAgent,
    session_id: &str,
    config: &splitlane_config::schema::SplitlaneConfig,
) -> Option<AgentCommandSpec> {
    if !crate::agent_sessions::is_valid_session_id(session_id) {
        log::warn!("resume_command: refused invalid session id, not sending to PTY");
        return None;
    }
    let spec = match agent {
        SessionAgent::Claude => {
            let mut spec = AgentCommandSpec::new("claude");
            spec.push_arg("--resume");
            spec.push_arg(session_id);
            if claude_bypass_enabled(config) {
                spec.push_arg("--permission-mode");
                spec.push_arg("bypassPermissions");
            }
            spec
        }
        SessionAgent::Codex => {
            let mut spec = AgentCommandSpec::new("codex");
            spec.push_arg("resume");
            spec.push_arg(session_id);
            spec
        }
        SessionAgent::OpenCode => {
            let mut spec = AgentCommandSpec::new("opencode");
            spec.push_arg("--session");
            spec.push_arg(session_id);
            spec
        }
        SessionAgent::Pi => {
            let mut spec = AgentCommandSpec::new("pi");
            spec.push_arg("--session");
            spec.push_arg(session_id);
            spec
        }
        SessionAgent::Hermes => resume_flag_spec("hermes", session_id),
        SessionAgent::Grok => resume_flag_spec("grok", session_id),
        SessionAgent::Cursor => {
            let mut spec = AgentCommandSpec::new("cursor-agent");
            spec.push_arg(format!("--resume={session_id}"));
            spec
        }
        SessionAgent::Gemini => resume_flag_spec("gemini", session_id),
        SessionAgent::Kiro => {
            let mut spec = AgentCommandSpec::new("kiro-cli");
            spec.push_arg("chat");
            spec.push_arg("--resume-id");
            spec.push_arg(session_id);
            spec
        }
    };
    debug_assert!(crate::agent_launcher::is_plain_shell_token(session_id));
    Some(spec)
}

fn resume_flag_spec(program: &'static str, session_id: &str) -> AgentCommandSpec {
    let mut spec = AgentCommandSpec::new(program);
    spec.push_arg("--resume");
    spec.push_arg(session_id);
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(summary: Option<&str>, id: &str) -> SessionMeta {
        SessionMeta {
            agent: SessionAgent::Claude,
            session_id: id.into(),
            last_activity_secs: 0,
            timestamp: "2026-07-05T12:00:00Z".into(),
            cwd: "/repo".into(),
            git_branch: String::new(),
            summary: summary.map(str::to_string),
            first_prompt: None,
            model: None,
            usage: None,
        }
    }

    #[test]
    fn empty_query_matches_every_session() {
        assert!(session_matches(&session(Some("Refactor auth"), "abc"), ""));
        assert!(session_matches(&session(None, "abc"), ""));
    }

    #[test]
    fn query_matches_title_case_insensitively() {
        let s = session(Some("Refactor the AUTH flow"), "abc");
        assert!(session_matches(&s, "auth"));
        assert!(session_matches(&s, "refactor"));
        assert!(!session_matches(&s, "payments"));
    }

    #[test]
    fn query_matches_session_id_for_untitled_rows() {
        // Untitled rows render their short id, so that is what the user has to
        // search on - matching only the (absent) title would make them
        // unreachable.
        let s = session(None, "550e8400-e29b-41d4-a716-446655440000");
        assert!(session_matches(&s, "550e8400"));
        assert!(!session_matches(&s, "nope"));
    }

    #[test]
    fn agent_index_is_stable() {
        for (idx, agent) in SessionAgent::ALL.into_iter().enumerate() {
            assert_eq!(agent_index(agent), idx);
        }
    }

    #[test]
    fn resume_command_neutralizes_flag_shaped_session_id() {
        // `resume_command` is the single builder that interpolates a
        // persisted/restored `session_id` into a PTY command line. It must
        // re-gate via `is_valid_session_id` so a flag-shaped value (one that
        // could inject e.g. `--dangerously-skip-permissions`) is refused at
        // the builder boundary - the call sites skip the send on `None`.
        // This proves the integration, not just the predicate
        // (`agent_sessions::valid_session_id_rejects_leading_dash_*`).
        let cfg = splitlane_config::schema::SplitlaneConfig::default();
        for agent in SessionAgent::ALL {
            assert_eq!(
                resume_command(agent, "--dangerously-skip-permissions", &cfg),
                None,
                "{agent:?}: a `--`-prefixed id must not build a command"
            );
            assert_eq!(resume_command(agent, "-x", &cfg), None);
            assert_eq!(resume_command(agent, "ses_x; rm -rf ~", &cfg), None);
            assert_eq!(resume_command(agent, "$(reboot)", &cfg), None);
        }
        // A legitimate UUID session id still builds a command for every agent.
        let valid = "019dc9ea-38d7-7372-9cc4-253ce944d41b";
        for agent in SessionAgent::ALL {
            assert!(resume_command(agent, valid, &cfg).is_some());
        }
    }

    #[test]
    fn resume_command_renders_expected_agent_commands() {
        let cfg = splitlane_config::schema::SplitlaneConfig::default();
        let id = "019dc9ea-38d7-7372-9cc4-253ce944d41b";

        let cases = [
            (SessionAgent::Claude, format!("claude --resume {id}")),
            (SessionAgent::Codex, format!("codex resume {id}")),
            (SessionAgent::OpenCode, format!("opencode --session {id}")),
            (SessionAgent::Pi, format!("pi --session {id}")),
            (SessionAgent::Hermes, format!("hermes --resume {id}")),
            (SessionAgent::Grok, format!("grok --resume {id}")),
            (SessionAgent::Cursor, format!("cursor-agent --resume={id}")),
            (SessionAgent::Gemini, format!("gemini --resume {id}")),
            (
                SessionAgent::Kiro,
                format!("kiro-cli chat --resume-id {id}"),
            ),
        ];

        for (agent, expected) in cases {
            assert_eq!(resume_command(agent, id, &cfg), Some(expected));
        }
    }

    #[test]
    fn resume_command_composes_claude_bypass_as_structured_args() {
        let cfg = splitlane_config::schema::SplitlaneConfig {
            claude_code_bypass_permissions: Some(true),
            ..Default::default()
        };
        let id = "019dc9ea-38d7-7372-9cc4-253ce944d41b";

        assert_eq!(
            resume_command(SessionAgent::Claude, id, &cfg),
            Some(format!(
                "claude --resume {id} --permission-mode bypassPermissions"
            ))
        );
    }

    #[test]
    fn visible_window_empty() {
        assert_eq!(visible_window(0, false, CAP), (0, 0));
    }

    #[test]
    fn visible_window_at_cap_has_no_remainder() {
        assert_eq!(visible_window(5, false, CAP), (5, 0));
    }

    #[test]
    fn visible_window_over_cap_caps_and_reports_remainder() {
        assert_eq!(visible_window(6, false, CAP), (5, 1));
        assert_eq!(visible_window(100, false, CAP), (5, 95));
    }

    #[test]
    fn visible_window_show_all_reveals_everything() {
        assert_eq!(visible_window(6, true, CAP), (6, 0));
        assert_eq!(visible_window(100, true, CAP), (100, 0));
    }

    #[test]
    fn older_sessions_hidden_label_pluralizes() {
        assert_eq!(older_sessions_hidden_label(1), "1 older session hidden");
        assert_eq!(older_sessions_hidden_label(2), "2 older sessions hidden");
    }

    #[test]
    fn scan_result_requires_matching_generation() {
        assert!(should_apply_scan_result(true, Some("/repo"), "/repo", 2, 2));
        assert!(
            !should_apply_scan_result(true, Some("/repo"), "/repo", 3, 2),
            "a stale same-cwd scan must not overwrite a newer open"
        );
        assert!(!should_apply_scan_result(
            false,
            Some("/repo"),
            "/repo",
            2,
            2
        ));
        assert!(!should_apply_scan_result(
            true,
            Some("/other"),
            "/repo",
            2,
            2
        ));
    }

    #[test]
    fn moved_session_selection_clamps_to_visible_rows() {
        assert_eq!(
            moved_session_selection(0, 3, SessionSelectionMove::Previous),
            0
        );
        assert_eq!(moved_session_selection(0, 3, SessionSelectionMove::Next), 1);
        assert_eq!(moved_session_selection(2, 3, SessionSelectionMove::Next), 2);
        assert_eq!(
            moved_session_selection(1, 3, SessionSelectionMove::First),
            0
        );
        assert_eq!(moved_session_selection(1, 3, SessionSelectionMove::Last), 2);
        assert_eq!(moved_session_selection(7, 0, SessionSelectionMove::Last), 0);
    }

    #[test]
    fn compact_cwd_label_uses_last_path_component() {
        assert_eq!(compact_cwd_label("/home/arthur/splitlane"), "splitlane");
        assert_eq!(compact_cwd_label("/home/arthur/splitlane/"), "splitlane");
        assert_eq!(compact_cwd_label(r"C:\dev\splitlane"), "splitlane");
        assert_eq!(compact_cwd_label("/"), "/");
    }
}
