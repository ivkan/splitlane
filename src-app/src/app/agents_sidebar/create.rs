//! The three ways to start work in a project, drawn at the foot of the
//! container that will hold it.
//!
//! This replaces a `+` on the container row that opened a menu of surface
//! *types*. In this app there are two kinds of session - an agent and a
//! shell - and everything else has its own way in: the diff opens from the
//! container's diffstat, a markdown document from the file tree, an old
//! session from the palette's History tab. A menu that listed all of them was
//! offering the rail as a table of contents for the whole app.
//!
//! `+ agent` is a menu rather than an assumption, because "which agent" is a
//! real question with a stable answer per container: the menu marks the
//! remembered one, picking another remembers it instead, and "Always ask"
//! clears the memory so `⌘N` comes back here rather than launching.
//!
//! `+ worktree` is the third, and it is here rather than behind a chord
//! because a worktree belongs to a repository and the rail is where a
//! repository is. It opens the worktree dialog; a project with no git repository
//! says so instead of opening an empty form.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Role, SharedString, StatefulInteractiveElement, Styled, Window, deferred, div, px,
};

use crate::SplitlaneApp;
use crate::agent_launcher::{PreferredAgent, TerminalAgent};
use crate::app::sidebar::context_menu::clamped_context_menu_position;
use crate::settings::components::select_menu;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// The agents this menu can offer.
///
/// "Only agents with a streaming mode can appear here - anything else is a
/// shell." Claude Code and Codex are the two the transport work covers; the
/// other fourteen launchers stay reachable as shells, which is what they
/// actually are to this app.
pub(crate) const STREAMING_AGENTS: [TerminalAgent; 2] =
    [TerminalAgent::ClaudeCode, TerminalAgent::Codex];

impl SplitlaneApp {
    /// `+ agent`, `+ shell` and `+ worktree`, dashed, at the foot of an
    /// expanded container.
    pub(super) fn new_session_buttons(
        &self,
        ws_idx: usize,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let container_id = self.workspaces.get(ws_idx).map(|ws| ws.id).unwrap_or(0);
        div()
            .flex()
            .flex_row()
            .gap(tok::space::SM)
            .ml(super::RAIL_SURFACE_MARGIN_L)
            .pl(super::RAIL_SURFACE_INDENT)
            .pr(super::RAIL_ROW_PADDING_X)
            .pt(tok::space::SM)
            .pb(tok::space::XS)
            .child(dashed_button(
                SharedString::from(format!("rail-new-agent-{container_id}")),
                "+ agent",
                "Start an agent session here",
                ui.accent,
                ui,
                cx.listener(move |this, e: &ClickEvent, _w, cx| {
                    let position = e
                        .mouse_position()
                        .unwrap_or_else(|| gpui::point(px(40.), px(80.)));
                    this.commit_agents_rename(cx);
                    this.open_new_agent_menu(ws_idx, position, cx);
                    cx.stop_propagation();
                }),
            ))
            .child(dashed_button(
                SharedString::from(format!("rail-new-shell-{container_id}")),
                "+ shell",
                "Open a shell here",
                ui.text_secondary,
                ui,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.close_agents_menu(cx);
                    this.commit_agents_rename(cx);
                    this.create_terminal_thread_in(ws_idx, cx);
                    cx.stop_propagation();
                }),
            ))
            .child(dashed_button(
                SharedString::from(format!("rail-new-worktree-{container_id}")),
                "+ worktree",
                "Create a git worktree for this project",
                // The branch role, the one the pane header uses for `\u{2442}`:
                // a worktree is a second checkout, and that is what the
                // colour says wherever it appears.
                ui.vc_modified,
                ui,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.close_agents_menu(cx);
                    this.commit_agents_rename(cx);
                    this.open_worktree_dialog(ws_idx, window, cx);
                    cx.stop_propagation();
                }),
            ))
            .into_any_element()
    }

    /// The `+ agent` menu: the streaming agents, then the entry that clears
    /// the container's remembered choice.
    pub(crate) fn render_new_agent_menu(
        &self,
        ws_idx: usize,
        position: gpui::Point<gpui::Pixels>,
        ui: crate::theme::UiColors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preferred = self.default_agent_for(ws_idx);
        // One row per agent and nothing else. The "Always ask which agent" row
        // that stood at the foot of this menu moved to the container's own
        // context menu with the rest of the choice: this menu **launches**, and
        // a menu that launches must not also decide what the chord does.
        let rows = STREAMING_AGENTS.len();
        let menu_height = px(8. + rows as f32 * 28.);
        let menu_pos = clamped_context_menu_position(position, px(240.), menu_height, window);

        let mut menu = select_menu("rail-new-agent-menu", ui)
            .occlude()
            .absolute()
            .left(menu_pos.x)
            .top(menu_pos.y)
            .w(px(240.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_agents_menu(cx);
            }))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());

        for agent in STREAMING_AGENTS {
            // The hint names the state, not just the chord: a container whose
            // default is Codex must not show `⌘N` beside Claude.
            let hint = if preferred == Some(agent) {
                Some(SharedString::from("default · ⌘N"))
            } else {
                None
            };
            menu = menu.child(self.render_select_menu_item(
                SharedString::from(format!("rail-new-agent-{}", agent.tag())),
                agent.display_name(),
                hint,
                ui,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.close_agents_menu(cx);
                    // Launch, and only launch. This used to write the
                    // container's default too.
                    this.create_agent_terminal_thread_in(ws_idx, agent, cx);
                    cx.stop_propagation();
                }),
            ));
        }

        deferred(menu).priority(3).into_any_element()
    }

    /// What the new-agent chord starts in this container, or `None` for "ask".
    ///
    /// The container's own answer, with the app-level `default_agent` under it
    /// for a container that has never been asked. Three states collapse to two
    /// here on purpose: this is the question every caller has, and "never
    /// asked" is not one of the answers to it.
    pub(crate) fn default_agent_for(&self, ws_idx: usize) -> Option<TerminalAgent> {
        self.workspaces
            .get(ws_idx)
            .and_then(|container| container.preferred_agent)
            .or_else(|| self.app_default_agent())
            .and_then(PreferredAgent::agent)
            // A named agent that is no longer here is not an answer. The tag
            // outlives the binary - the CLI is uninstalled, or turned off in
            // Settings - and without this the chord launched something that
            // could not start while the menu ticked nothing, because the menu
            // only lists agents on PATH. Falling through to "ask" is the same
            // answer a project that never chose gets, which is the truth of it.
            .filter(|agent| agent.is_visible(&self.cached_config) && agent.is_installed())
    }

    /// The app-level fallback, from `splitlane.json`. Absent means the chord
    /// asks - the only honest answer before anyone has chosen.
    pub(crate) fn app_default_agent(&self) -> Option<PreferredAgent> {
        self.cached_config
            .default_agent
            .as_deref()
            .and_then(PreferredAgent::from_tag)
    }

    /// Write which agent this container starts by default.
    ///
    /// **Only a choice made to write it calls this.** Launching an agent must
    /// not: it used to be called from the `+ agent` menu's own click, so every
    /// pick re-pointed the chord, and a cross-vendor run through a second agent
    /// left it there permanently and silently.
    pub(crate) fn set_preferred_agent(
        &mut self,
        ws_idx: usize,
        preferred: Option<PreferredAgent>,
        cx: &mut Context<Self>,
    ) {
        let Some(container) = self.workspaces.get_mut(ws_idx) else {
            return;
        };
        if container.preferred_agent == preferred {
            return;
        }
        container.preferred_agent = preferred;
        self.save_session(cx);
        cx.notify();
    }
}

/// One dashed affordance. Dashed because it is a place a row *will* be, not a
/// row: the design uses the same outline for the launch-pad presets and for
/// the permission block's suggested answers.
fn dashed_button(
    id: SharedString,
    label: &'static str,
    tooltip: &'static str,
    hover_text: gpui::Hsla,
    ui: crate::theme::UiColors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let border_resting = ui.border_strong;
    let border_hover = ui.accent_border;
    let text_resting = ui.dim;
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .px(tok::space::SM)
        .py(px(3.))
        .rounded(tok::radius::TAG)
        .border_1()
        .border_dashed()
        .border_color(border_resting)
        .font_family(tok::font::MONO)
        .text_size(tok::mono::LABEL)
        .text_color(text_resting)
        .tooltip(crate::ui_primitives::text_tooltip(tooltip))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .cursor_pointer()
        .on_click(on_click)
        .animated_hover(move |style, delta| {
            style
                .border_color(lerp_color(border_resting, border_hover, delta))
                .text_color(lerp_color(text_resting, hover_text, delta));
        })
        .child(label)
        .into_any_element()
}
