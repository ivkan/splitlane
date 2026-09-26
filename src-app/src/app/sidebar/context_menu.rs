//! Context-menu row helpers shared between the sidebar workspace menu and the
//! title-bar burger menu. Includes the action-name shortcut lookup used
//! to render the keyboard-shortcut label next to each action.
//!
//! Part of the sidebar decomposition.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, SharedString, Styled, Window, deferred, div, point,
    prelude::*, px,
};

use crate::agent_launcher::{PreferredAgent, TerminalAgent};
use crate::app::files_tree;
use crate::pane::TabContent;
use crate::settings::components::{menu_divider_color, select_item, select_menu, with_alpha};
use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;
use crate::{SplitlaneApp, TabContextMenu, WorkspaceContextMenu};

pub(crate) const EDITOR_CONTEXT_MENU_ITEMS: &[(&str, &str, &str, &str)] = &[
    ("zed", "Open in Zed", "zed", "open_workspace_in_zed"),
    (
        "cursor",
        "Open in Cursor",
        "cursor",
        "open_workspace_in_cursor",
    ),
    (
        "vscode",
        "Open in VS Code",
        "code",
        "open_workspace_in_vscode",
    ),
    (
        "windsurf",
        "Open in Windsurf",
        "windsurf",
        "open_workspace_in_windsurf",
    ),
];

fn context_menu_divider(ui: crate::theme::UiColors) -> gpui::Div {
    div()
        .mx(tok::space::XS)
        .my(tok::space::XS)
        .flex_none()
        .h(px(1.))
        .bg(menu_divider_color(ui))
}

pub(crate) fn clamped_context_menu_position(
    position: gpui::Point<Pixels>,
    width: Pixels,
    height: Pixels,
    window: &Window,
) -> gpui::Point<Pixels> {
    let win_size = window.window_bounds().get_bounds().size;
    let x = if position.x + width > win_size.width {
        (position.x - width).max(px(0.))
    } else {
        position.x
    };
    let y = if position.y + height > win_size.height {
        (position.y - height).max(px(0.))
    } else {
        position.y
    };
    point(x, y)
}

impl SplitlaneApp {
    pub(crate) fn shortcut_for_action(&self, action_name: &str) -> Option<&str> {
        self.effective_shortcuts
            .iter()
            .find(|entry| entry.action_name == action_name && entry.key != "Unassigned")
            .map(|entry| entry.key.as_str())
    }

    pub(crate) fn render_context_menu_item(
        &self,
        id: SharedString,
        label: &str,
        shortcut: Option<SharedString>,
        ui: crate::theme::UiColors,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_between()
            .gap(tok::space::MD)
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::BADGE)
            .text_size(tok::text::CAPTION)
            .text_color(ui.text)
            .animated_hover_bg(ui.subtle.opacity(0.0), ui.subtle)
            .cursor_pointer()
            .on_click(on_click)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label.to_string()),
            )
            .when_some(shortcut, |d, shortcut| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.muted)
                        .child(shortcut),
                )
            })
    }

    /// One workspace-menu row in the shared Settings "Shell" select look
    /// (`components::select_item`): 28px tall, 7px radius, 12px label flex-filled
    /// with the optional shortcut pinned right, and the whisper hover highlight
    /// (`text @ 0.05`) instead of the older flat `ui.subtle`. Keeps every app
    /// menu reading as one consistent menu language.
    pub(crate) fn render_select_menu_item(
        &self,
        id: SharedString,
        label: &str,
        shortcut: Option<SharedString>,
        ui: crate::theme::UiColors,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        select_item(id, false, ui)
            .on_click(on_click)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(ui.text)
                    .child(label.to_string()),
            )
            .when_some(shortcut, |d, shortcut| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.muted)
                        .child(shortcut),
                )
            })
    }

    pub(crate) fn open_workspace_service_url(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Err(err) = crate::external_open::open_url(url) {
            let message = if err.kind() == std::io::ErrorKind::NotFound {
                "Could not open URL - install xdg-utils (Linux), or check your default browser"
                    .to_string()
            } else {
                format!("Could not open URL: {err}")
            };
            log::warn!("sidebar: open URL failed: {err}");
            self.show_toast(message, cx);
        }
    }

    /// Build the deferred element that paints the right-click workspace
    /// context menu. Caller is responsible for the
    /// `if let Some(menu) = self.workspace_menu_open && menu.idx < self.workspaces.len()`
    /// guard. Extracted from `main.rs`.
    pub(crate) fn render_workspace_context_menu(
        &self,
        menu: WorkspaceContextMenu,
        ui: crate::theme::UiColors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let idx = menu.idx;
        let can_close = !self.workspaces.is_empty();
        let project_preset = self.preset_for_project(idx);
        let services: Vec<_> = self
            .workspaces
            .get(idx)
            .map(|workspace| {
                workspace
                    .active_ports
                    .iter()
                    .filter_map(|port| {
                        workspace
                            .service_labels
                            .get(port)
                            .cloned()
                            .map(|info| (*port, info))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let is_git_repo = self
            .workspaces
            .get(idx)
            .is_some_and(|workspace| workspace.is_git_repo);
        // The rows below are named after the chord, so they have to say the
        // chord this build actually binds - it is per-platform, and a user's
        // own `shortcuts` map can move it.
        let new_agent_chord = self
            .shortcut_for_action("new_agent")
            .map(|chord| chord.to_string())
            .unwrap_or_else(|| "New agent".to_string());
        let installed_agents = TerminalAgent::ALL
            .iter()
            .copied()
            .filter(|agent| agent.is_visible(&self.cached_config) && agent.is_installed())
            .count();
        let preset_rows = usize::from(project_preset.is_some());
        let branch_rows = usize::from(is_git_repo);
        let service_rows = services.len();
        let separator_rows = 5 + usize::from(service_rows > 0);
        // Counted, not guessed: this height is what decides whether the menu
        // is flipped above the pointer near the window's bottom edge, so an
        // undercount drops the last rows off the screen. The fixed part is
        // nine - new agent session, new shell, resume, rename, reveal, copy
        // path, custom buttons, close project, and the branch/service header
        // - and it was written as eight.
        let menu_rows = EDITOR_CONTEXT_MENU_ITEMS.len()
            + 10
            + preset_rows
            + branch_rows
            + service_rows
            // The chord group: one row per installed agent, plus "always asks".
            + installed_agents
            + 1;
        let menu_height = px(8. + menu_rows as f32 * 28. + separator_rows as f32 * 9.);
        let menu_pos = clamped_context_menu_position(menu.position, px(248.), menu_height, window);
        // `Add to group ▸` sits under the three creation rows, a divider and
        // `Rename`; its submenu opens level with it. Arithmetic, like the
        // height above, because both are needed before layout.
        let submenu = menu
            .group_submenu
            .then(|| self.group_submenu_bounds(idx, menu_pos, window));
        let mut context_menu = select_menu("workspace-context-menu", ui)
            .occlude()
            .absolute()
            .left(menu_pos.x)
            .top(menu_pos.y)
            .w(px(248.))
            .on_mouse_down_out(cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                // The submenu is outside this menu's bounds but part of it.
                if submenu.is_some_and(|bounds| bounds.contains(&e.position)) {
                    return;
                }
                this.workspace_menu_open = None;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());

        // Creation first: what the Agents rail's own project menu carried
        // before the two menus merged. One object, one menu: an action lives
        // where its object lives, and a container's actions live on its row.
        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-new-thread".into(),
            "New agent session",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.workspace_menu_open = None;
                this.create_agents_thread_in(idx, cx);
                cx.stop_propagation();
            }),
        ));
        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-new-terminal-thread".into(),
            "New shell",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.workspace_menu_open = None;
                this.create_terminal_thread_in(idx, cx);
                cx.stop_propagation();
            }),
        ));
        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-resume-session".into(),
            "Resume session…",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.workspace_menu_open = None;
                this.open_sessions_sidebar_for_project(idx, Some(window), cx);
                cx.stop_propagation();
            }),
        ));

        context_menu = context_menu.child(context_menu_divider(ui));

        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-rename".into(),
            "Rename",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.workspace_menu_open = None;
                this.begin_workspace_rename(idx, cx);
                cx.stop_propagation();
                cx.notify();
            }),
        ));

        // `Add to group ▸`, always a submenu, even with no groups yet and one
        // entry in it: an item has one place, or the hand looks for it in two.
        context_menu = context_menu.child(
            select_item("workspace-context-add-to-group", menu.group_submenu, ui)
                .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                    if *hovered && let Some(open) = this.workspace_menu_open.as_mut() {
                        open.group_submenu = true;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if let Some(open) = this.workspace_menu_open.as_mut() {
                        open.group_submenu = !open.group_submenu;
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_x_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(ui.text)
                        .child("Add to group"),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.muted)
                        .child("\u{25b8}"),
                ),
        );

        // Switch branch. The branch is a property of the checkout, so its
        // picker hangs off the container's own menu. It used to be reachable
        // only from the environment card floating over an agent surface -
        // a container action living inside one surface's chrome, and invisible
        // from every other surface of the same container.
        if is_git_repo {
            context_menu = context_menu.child(self.render_select_menu_item(
                "workspace-context-switch-branch".into(),
                "Switch branch…",
                None,
                ui,
                cx.listener(move |this, e: &ClickEvent, window, cx| {
                    let anchor = e.mouse_position().unwrap_or_default();
                    this.workspace_menu_open = None;
                    this.open_agents_branch_menu_for_project(idx, anchor, window, cx);
                    cx.stop_propagation();
                }),
            ));
        }

        // "Run preset", not "Run Workflow": a workflow is a fourth name for
        // the thing the rest of the interface calls a preset, and this row
        // was the only place it appeared.
        if let Some(preset_idx) = project_preset {
            context_menu = context_menu.child(self.render_select_menu_item(
                "workspace-context-run-preset".into(),
                "Run preset",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.workspace_menu_open = None;
                    let stored = this.presets.get(preset_idx).cloned();
                    this.run_preset_for_project(stored, idx, cx);
                    cx.stop_propagation();
                }),
            ));
        }

        context_menu = context_menu.child(context_menu_divider(ui));

        for (port, info) in services {
            let service_name = info
                .label
                .clone()
                .unwrap_or_else(|| "Local service".to_string());
            if info.is_frontend {
                let label = format!("Open {service_name} :{port}");
                let url = info
                    .url
                    .clone()
                    .unwrap_or_else(|| format!("http://localhost:{port}"));
                context_menu = context_menu.child(self.render_select_menu_item(
                    SharedString::from(format!("workspace-context-service-{port}")),
                    &label,
                    None,
                    ui,
                    cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.workspace_menu_open = None;
                        this.open_workspace_service_url(&url, cx);
                        cx.stop_propagation();
                    }),
                ));
            } else {
                context_menu = context_menu.child(Self::render_disabled_select_menu_item(
                    SharedString::from(format!("workspace-context-service-{port}-info")),
                    &format!("{service_name} :{port}"),
                    ui,
                ));
            }
        }

        if service_rows > 0 {
            context_menu = context_menu.child(context_menu_divider(ui));
        }

        for &(id, label, command, shortcut_action) in EDITOR_CONTEXT_MENU_ITEMS {
            let shortcut = self
                .shortcut_for_action(shortcut_action)
                .map(|s| SharedString::from(s.to_string()));
            let command = command.to_string();
            let label_owned = label.to_string();
            context_menu = context_menu.child(self.render_select_menu_item(
                SharedString::from(format!("workspace-context-{id}")),
                label,
                shortcut,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.open_workspace_in_editor(idx, &command, &label_owned, cx);
                    cx.stop_propagation();
                }),
            ));
        }

        context_menu = context_menu.child(context_menu_divider(ui));

        // Reveal in file manager
        let reveal_shortcut = self
            .shortcut_for_action("reveal_workspace_in_file_manager")
            .map(|s| SharedString::from(s.to_string()));
        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-reveal".into(),
            "Reveal in File Manager",
            reveal_shortcut,
            ui,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.reveal_workspace_in_file_manager(idx, cx);
                cx.stop_propagation();
            }),
        ));

        // Copy path
        let copy_shortcut = self
            .shortcut_for_action("copy_workspace_path")
            .map(|s| SharedString::from(s.to_string()));
        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-copy".into(),
            "Copy Path",
            copy_shortcut,
            ui,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.copy_workspace_path(idx, cx);
                cx.stop_propagation();
            }),
        ));

        // Manage Custom Buttons - opens the per-workspace button editor modal.
        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-custom-buttons".into(),
            "Manage Custom Buttons…",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_custom_buttons_modal(idx, window, cx);
                cx.stop_propagation();
            }),
        ));

        context_menu = context_menu.child(context_menu_divider(ui));

        // What the new-agent chord starts in this project, and the **only**
        // place that writes it. Named after the chord rather than
        // after "default agent", because the chord is the thing the user
        // presses and "default" is our word for a mechanism.
        //
        // The tick is the **effective** value, so a project that has never
        // been asked shows it on whatever Settings says. Picking a row writes
        // the project's own answer; there is no way back to "inherit" from
        // here, which is the design's own shape - the fallback is where an
        // unanswered project starts, not a state to return to.
        //
        // Only agents actually on PATH: a row that starts nothing is worse
        // than an absent row, which is the same rule the pane launcher keeps.
        let chord_agent = self.default_agent_for(idx);
        // Ticked when the effective answer is "ask", which includes the case
        // nobody has answered at all - asking is what the chord then does, and
        // a menu where no row is ticked says the state is unknown when it is
        // not. Asked of the same resolver the agent rows use, so the tick
        // cannot land on two rows or on none.
        let asks = chord_agent.is_none();
        for agent in TerminalAgent::ALL
            .iter()
            .copied()
            .filter(|agent| agent.is_visible(&self.cached_config) && agent.is_installed())
        {
            let ticked = chord_agent == Some(agent);
            context_menu = context_menu.child(self.render_select_menu_item(
                SharedString::from(format!("workspace-context-chord-{}", agent.tag())),
                &format!("{new_agent_chord} runs {}", agent.display_name()),
                ticked.then(|| SharedString::from("\u{2713}")),
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.workspace_menu_open = None;
                    this.set_preferred_agent(idx, Some(PreferredAgent::Agent(agent)), cx);
                    cx.stop_propagation();
                }),
            ));
        }
        context_menu = context_menu.child(self.render_select_menu_item(
            "workspace-context-chord-ask".into(),
            &format!("{new_agent_chord} always asks"),
            asks.then(|| SharedString::from("\u{2713}")),
            ui,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.workspace_menu_open = None;
                this.set_preferred_agent(idx, Some(PreferredAgent::Ask), cx);
                cx.stop_propagation();
            }),
        ));

        context_menu = context_menu.child(context_menu_divider(ui));

        // Close workspace (conditionally disabled)
        let close_shortcut = self
            .shortcut_for_action("close_workspace")
            .map(|s| SharedString::from(s.to_string()));
        context_menu = context_menu.child({
            let hover_bg = with_alpha(ui.text, 0.05);
            let target_bg = if can_close {
                hover_bg
            } else {
                hover_bg.opacity(0.0)
            };
            div()
                .id("workspace-context-close")
                .flex_none()
                .h(crate::app::constants::LIST_ROW_HEIGHT)
                .px(tok::space::MD)
                .rounded(tok::radius::CONTROL)
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::MD)
                .text_size(tok::text::ROW)
                .text_color(ui.muted)
                .when(can_close, |d| d.text_color(ui.text))
                .animated_hover_bg(hover_bg.opacity(0.0), target_bg)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.workspace_menu_open = None;
                    if can_close {
                        // Through the confirmation, because a container now
                        // holds agent surfaces too: closing it takes their
                        // PTYs with it.
                        this.request_delete_for_target(
                            crate::project::AgentsTarget::Panes { ws_idx: idx },
                            cx,
                        );
                    } else {
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_x_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child("Close Project"),
                )
                .when_some(close_shortcut, |d, shortcut| {
                    d.child(
                        div()
                            .flex_none()
                            .text_size(tok::text::CAPTION)
                            .text_color(ui.muted)
                            .child(shortcut),
                    )
                })
        });

        let context_menu = deferred(context_menu).priority(3).into_any_element();
        match submenu {
            Some(bounds) => div()
                .child(context_menu)
                .child(self.render_group_submenu(idx, bounds, ui, cx))
                .into_any_element(),
            None => context_menu,
        }
    }

    /// Where `Add to group ▸`'s submenu goes: level with its row, to the
    /// right of the menu, or to its left where the window ends.
    fn group_submenu_bounds(
        &self,
        idx: usize,
        menu_pos: gpui::Point<Pixels>,
        window: &Window,
    ) -> gpui::Bounds<Pixels> {
        const MENU_W: f32 = 248.;
        const SUB_W: f32 = 196.;
        // Rows are 28 with a 1px gap; a divider is 9 with the same gap.
        let row = 29.;
        let row_top = 4. + 3. * row + 10. + row;
        let rows = self.live_project_groups().len() as f32
            + 1.
            + f32::from(u8::from(self.group_of_project(idx).is_some()));
        let dividers = if self.live_project_groups().is_empty() {
            0.
        } else {
            10.
        };
        let height = 8. + rows * row + dividers;
        let win = window.window_bounds().get_bounds().size;
        let right = menu_pos.x + px(MENU_W - 4.);
        let x = if right + px(SUB_W) > win.width {
            (menu_pos.x - px(SUB_W - 4.)).max(px(0.))
        } else {
            right
        };
        let y = (menu_pos.y + px(row_top - 4.)).min((win.height - px(height)).max(px(0.)));
        gpui::Bounds::new(point(x, y), gpui::size(px(SUB_W), px(height)))
    }

    /// `Add to group ▸`: the groups in rail order with the project's own
    /// ticked, then `New group…`, then `Remove from group` for a project that
    /// is in one. Names as typed - the capitals are the rail label's style.
    fn render_group_submenu(
        &self,
        idx: usize,
        bounds: gpui::Bounds<Pixels>,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.group_of_project(idx).map(|group| group.id);
        let groups: Vec<(u64, String)> = self
            .live_project_groups()
            .into_iter()
            .map(|group| (group.id, group.name.clone()))
            .collect();
        let tick_column = |ticked: bool| {
            div()
                .flex_none()
                .w(tok::space::LG)
                .font_family(tok::font::MONO)
                .text_size(tok::mono::ROW)
                .text_color(ui.text)
                .when(ticked, |d| d.child("\u{2713}"))
        };
        let mut sub = select_menu("workspace-context-group-submenu", ui)
            .occlude()
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .min_w(bounds.size.width)
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        let has_groups = !groups.is_empty();
        for (group_id, name) in groups {
            let ticked = current == Some(group_id);
            sub = sub.child(
                select_item(
                    SharedString::from(format!("workspace-context-group-{group_id}")),
                    false,
                    ui,
                )
                .gap(tok::space::MD)
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.workspace_menu_open = None;
                    // The ticked one is where the project already is.
                    this.add_project_to_group(idx, group_id, cx);
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(tick_column(ticked))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::ROW)
                        .text_color(ui.text)
                        .child(name),
                ),
            );
        }
        if has_groups {
            sub = sub.child(context_menu_divider(ui));
        }
        sub = sub.child(
            select_item("workspace-context-new-group", false, ui)
                .gap(tok::space::MD)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.workspace_menu_open = None;
                    this.start_new_group_with(idx, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(tick_column(false))
                .child(div().text_color(ui.text).child("New group\u{2026}")),
        );
        if current.is_some() {
            sub = sub.child(
                select_item("workspace-context-remove-from-group", false, ui)
                    .gap(tok::space::MD)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.workspace_menu_open = None;
                        this.remove_project_from_group(idx, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(tick_column(false))
                    .child(div().text_color(ui.text).child("Remove from group")),
            );
        }
        deferred(sub).priority(4).into_any_element()
    }

    /// The menu the slot header's overflow button opens: what can be done to
    /// the surface this pane is showing.
    ///
    /// It used to lead with a "Move to Pane N" entry per other pane. That
    /// action lives on the rail row now, as "Show in <what that pane holds>",
    /// because panes are numbered nowhere a person can see: a menu asking for
    /// a number asks them to guess. Two doors to one action, under two
    /// different words, is the drift this project keeps closing.
    pub(crate) fn render_tab_context_menu(
        &self,
        menu: TabContextMenu,
        ui: crate::theme::UiColors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let source = menu.source_pane.clone();
        let source_idx = source.read(cx).index_for_tab_id(menu.tab_id);

        // The container this pane belongs to, for the relative path below.
        let workspace_cwd: Option<PathBuf> = self.workspaces.iter().find_map(|ws| {
            let root = ws.root.as_ref()?;
            root.contains_leaf(&source).then(|| PathBuf::from(&ws.cwd))
        });

        let tab_path = source
            .read(cx)
            .tabs
            .get(source_idx.unwrap_or(usize::MAX))
            .and_then(|tab| Self::tab_context_path(tab, cx));
        let full_path = tab_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let relative_path = tab_path.as_ref().map(|path| {
            workspace_cwd
                .as_ref()
                .map(|root| files_tree::workspace_relative_path(root, path))
                .unwrap_or_else(|| path.to_string_lossy().into_owned())
        });

        // Cancel this tab's queued prompt -
        // the non-Composer cancel path. Only shown when a buffer exists.
        let pending_sid = source
            .read(cx)
            .tabs
            .get(source_idx.unwrap_or(usize::MAX))
            .and_then(|t| t.as_terminal())
            .map(|t| t.entity_id().as_u64())
            .filter(|sid| self.broadcast.pending.contains_key(sid));

        // The surface's own operations, which used to be a row of unlabelled
        // icons in the header. `surface_ref_id` is the value `list_panes`
        // reports, so what is copied here is what an agent can be told to read.
        let active_tab = source
            .read(cx)
            .tabs
            .get(source_idx.unwrap_or(usize::MAX))
            .cloned();
        let active_terminal = active_tab
            .as_ref()
            .and_then(|tab| tab.as_terminal().cloned());
        // The two path actions the file card already offered, in the menu that
        // is the surface's own home - and the section this menu drew a rule
        // above and then left empty on a document.
        //
        // "Reveal" is asked of a **directory**: on macOS `open <file>` hands
        // the file to its default application rather than showing it, which is
        // the other row. So a terminal reveals the directory it is already in,
        // and a document reveals the one it sits in.
        let reveal_dir: Option<PathBuf> = tab_path.as_ref().and_then(|path| {
            if active_terminal.is_some() {
                Some(path.clone())
            } else {
                path.parent().map(PathBuf::from)
            }
        });
        // A document only. For a terminal this would name the directory the
        // row above already names, under a word that promises something else.
        //
        // The **label** is the file's to decide: a `.png` has no editor to be
        // opened in, and a `package.json` handed to the OS opens in whatever
        // claimed the extension rather than in the editor the person chose in
        // Settings. One row either way, saying which of the two it is.
        let open_file: Option<(PathBuf, bool)> = match &active_tab {
            Some(TabContent::Markdown(view)) => tab_path
                .clone()
                .map(|path| (path, view.read(cx).opens_in_an_editor())),
            _ => None,
        };
        let surface_ref_id = active_terminal.as_ref().map(|t| t.entity_id().as_u64());
        // "Copy the last answer" needs to know WHICH conversation on disk is
        // this surface's, so it appears only for a surface that is bound to
        // one. A surface whose CLI has not reported a session yet - or whose
        // CLI never does - would otherwise offer an entry that can only fail.
        //
        // The question is asked of `claude_sessions::transcript_path`, the one
        // home for "whose file can this build read". It used to be asked of
        // `Thread::agent == ClaudeCode`, which looks like the same question and
        // is not one at all: `Thread::terminal` hardcodes that field to
        // `ClaudeCode` for every terminal surface whatever CLI it launches, so
        // the filter passed always. Seen by eye - the entry was offered on a
        // Codex surface, where it can only build a Claude-shaped path that does
        // not exist and report "no answer to copy yet", which is a claim about
        // that session rather than about the limits of this build.
        let answer_thread = active_terminal
            .as_ref()
            .and_then(|t| t.read(cx).agent_thread_id)
            .and_then(|thread_id| self.thread_by_id(thread_id))
            .filter(|thread| crate::claude_sessions::transcript_path(thread).is_some());
        let answer_source = answer_thread.and_then(|thread| {
            Some((
                thread.session_id.clone()?,
                std::path::PathBuf::from(&thread.cwd),
            ))
        });
        // The same answer, sent to another agent on screen instead of the
        // clipboard. Offered wherever the copy is, so the two menus of one
        // surface say the same things.
        let send_answer: Option<(u64, Vec<crate::app::send_answer::AnswerDestination>)> =
            answer_thread
                .filter(|_| answer_source.is_some())
                .map(|thread| (thread.id, self.answer_destinations(thread.id, cx)));
        // The surface this pane is showing, when it has a rail row at all. A
        // markdown document and the diff have no record to delete - closing
        // them loses nothing, which is exactly what "Close pane" already does.
        //
        // Its **id**, not its position. `AgentsTarget::Thread` is a pair of
        // indices, and this menu's rows are built one frame and clicked the
        // next; a surface removed from the same container in between - by
        // another pane, by the rail, by a sweep - shifts every index after it,
        // and a captured pair would then name a different session to a
        // confirmation dialog that deletes it. The id is resolved again inside
        // the click, and nothing happens if it no longer resolves.
        let delete_thread_id = active_terminal
            .as_ref()
            .and_then(|t| t.read(cx).agent_thread_id)
            .filter(|thread_id| {
                crate::project::find_surface(&self.workspaces, *thread_id).is_some()
            });
        // Hidden for the same reason the icon was: with every agent toggled off
        // in Settings the dock would open empty.
        let show_sessions_entry = active_terminal.is_some()
            && !crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config)
                .is_empty();
        let custom_buttons: Vec<splitlane_config::schema::ButtonCommand> = self
            .workspaces
            .iter()
            .find(|ws| ws.contains_pane(&source))
            .map(|ws| ws.custom_buttons.clone())
            .unwrap_or_default();

        // A divider is a claim that there is something on both sides of it.
        // Every section below the path rows is conditional - a document has no
        // agent operations, and most surfaces have neither the "no longer here"
        // note nor a queued prompt - and a divider emitted regardless drew a
        // rule under nothing. On a file surface that was two rules in a row
        // between `Copy Relative Path` and `Collapse to this pane`, which is
        // what the report called strange. The last divider needs no guard:
        // `Close pane` is always under it.
        let has_agent_ops =
            surface_ref_id.is_some() || answer_source.is_some() || show_sessions_entry;
        let has_pane_notices = source_idx.is_none() || pending_sid.is_some();

        let extra_rows = usize::from(reveal_dir.is_some())
            + usize::from(open_file.is_some())
            + usize::from(surface_ref_id.is_some())
            + usize::from(show_sessions_entry)
            + usize::from(answer_source.is_some())
            + send_answer
                .as_ref()
                .map_or(0, |(_, destinations)| destinations.len())
            + usize::from(delete_thread_id.is_some())
            + custom_buttons.len();
        // Whether this pane's container has anything to collapse. Asked of the
        // container that holds **this** pane rather than the active one: the
        // menu is the pane's, and a pane can outlive the selection that opened
        // it.
        let more_than_one_pane = self
            .workspaces
            .iter()
            .find(|ws| ws.contains_pane(&source))
            .and_then(|ws| ws.root.as_ref())
            .is_some_and(|root| root.leaf_count() > 1);
        // The "no longer here" note is a row like any other: it was
        // counted by the old `others.len().max(1)` term, which stood for
        // either the move entries or that one line, and both halves went
        // when the move entries did.
        let rows = 3
            + usize::from(source_idx.is_none())
            + usize::from(pending_sid.is_some())
            + usize::from(more_than_one_pane)
            + extra_rows;
        // The dividers are part of the height the clamp is asked about, and
        // they were not counted at all - which put a tall menu's last rows
        // below the window edge by as much as three of them. Each is 1px of
        // rule between two `space::XS` margins.
        let dividers = 1
            + usize::from(has_agent_ops)
            + usize::from(has_pane_notices)
            + usize::from(!custom_buttons.is_empty());
        let menu_height = px(8. + rows as f32 * 29. + dividers as f32 * 9. + 18.);
        let menu_pos = clamped_context_menu_position(menu.position, px(248.), menu_height, window);

        let mut context_menu = select_menu("tab-context-menu", ui)
            .occlude()
            .absolute()
            .left(menu_pos.x)
            .top(menu_pos.y)
            .w(px(248.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.tab_menu_open = None;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());

        if let Some(value) = full_path {
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-copy-path".into(),
                "Copy Path",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                    this.tab_menu_open = None;
                    this.show_toast("Copied path", cx);
                    cx.stop_propagation();
                }),
            ));
        } else {
            context_menu = context_menu.child(Self::render_disabled_select_menu_item(
                "tab-context-copy-path-disabled".into(),
                "Copy Path unavailable",
                ui,
            ));
        }

        if let Some(value) = relative_path {
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-copy-relative-path".into(),
                "Copy Relative Path",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                    this.tab_menu_open = None;
                    this.show_toast("Copied relative path", cx);
                    cx.stop_propagation();
                }),
            ));
        } else {
            context_menu = context_menu.child(Self::render_disabled_select_menu_item(
                "tab-context-copy-relative-path-disabled".into(),
                "Copy Relative Path unavailable",
                ui,
            ));
        }

        if let Some(dir) = reveal_dir {
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-reveal".into(),
                "Reveal in file manager",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab_menu_open = None;
                    if let Err(err) = crate::app::workspace_ops::reveal_in_file_manager(&dir) {
                        this.show_toast(err, cx);
                    }
                    cx.stop_propagation();
                }),
            ));
        }

        if let Some((path, in_editor)) = open_file {
            let hint: Option<SharedString> = in_editor.then(|| "\u{23ce}".into());
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-open-default".into(),
                if in_editor {
                    "Open in editor"
                } else {
                    "Open with the default app"
                },
                hint,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab_menu_open = None;
                    if in_editor {
                        this.open_path_in_editor(path.clone(), cx);
                    } else if let Err(err) = crate::app::workspace_ops::open_in_default_app(&path) {
                        this.show_toast(err, cx);
                    }
                    cx.stop_propagation();
                }),
            ));
        }

        // The surface's own operations, moved here from the header's action
        // cluster. An action lives where its object lives, and this
        // menu IS the surface's home; the header itself went back to saying
        // what the pane is showing rather than what can be done to it.
        if has_agent_ops {
            context_menu = context_menu.child(
                div()
                    .mx(tok::space::XS)
                    .my(tok::space::XS)
                    .flex_none()
                    .h(px(1.))
                    .bg(menu_divider_color(ui)),
            );
        }

        if let Some(surface_id) = surface_ref_id {
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-copy-surface-ref".into(),
                "Copy Surface Reference",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab_menu_open = None;
                    this.copy_surface_ref(surface_id, cx);
                    cx.stop_propagation();
                }),
            ));
        }

        if let Some((session_id, cwd)) = answer_source {
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-copy-last-answer".into(),
                "Copy the Last Answer",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab_menu_open = None;
                    this.copy_last_answer(session_id.clone(), cwd.clone(), cx);
                    cx.stop_propagation();
                }),
            ));
        }

        if let Some((source_thread_id, destinations)) = send_answer {
            for (idx, destination) in destinations.into_iter().enumerate() {
                let hint =
                    (!destination.slot.is_empty()).then(|| SharedString::from(destination.slot));
                context_menu = context_menu.child(self.render_select_menu_item(
                    SharedString::from(format!("tab-context-send-answer-{idx}")),
                    &format!("Send the Last Answer to {}", destination.label),
                    hint,
                    ui,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.tab_menu_open = None;
                        this.send_last_answer(source_thread_id, destination.clone(), window, cx);
                        cx.stop_propagation();
                    }),
                ));
            }
        }

        if show_sessions_entry {
            let source_for_sessions = source.clone();
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-agent-sessions".into(),
                "Agent Session History\u{2026}",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab_menu_open = None;
                    this.open_sessions_sidebar_for_pane(&source_for_sessions, None, cx);
                    cx.stop_propagation();
                }),
            ));
        }

        // The container's own command buttons. They had no home in the
        // design at all, and deleting someone's configured commands to
        // match a mockup is not ours to do - so they land here, in a group
        // of their own, still one click from the surface they type into.
        if !custom_buttons.is_empty() {
            context_menu = context_menu.child(
                div()
                    .mx(tok::space::XS)
                    .my(tok::space::XS)
                    .flex_none()
                    .h(px(1.))
                    .bg(menu_divider_color(ui)),
            );
            for button in custom_buttons {
                let command = button.command.clone();
                let source_for_command = source.clone();
                let tab_id = menu.tab_id;
                context_menu = context_menu.child(self.render_select_menu_item(
                    SharedString::from(format!("tab-context-command-{}", button.id)),
                    &button.name,
                    None,
                    ui,
                    cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.tab_menu_open = None;
                        if let Some(terminal) = source_for_command
                            .read(cx)
                            .tabs
                            .iter()
                            .find(|tab| tab.entity_id() == tab_id)
                            .and_then(|tab| tab.as_terminal())
                        {
                            terminal.read(cx).send_command(&command);
                        }
                        cx.stop_propagation();
                    }),
                ));
            }
        }

        if has_pane_notices {
            context_menu = context_menu.child(
                div()
                    .mx(tok::space::XS)
                    .my(tok::space::XS)
                    .flex_none()
                    .h(px(1.))
                    .bg(menu_divider_color(ui)),
            );
        }

        if source_idx.is_none() {
            context_menu = context_menu.child(
                div()
                    .px(tok::space::MD)
                    .py(tok::space::XS)
                    .rounded(tok::radius::BADGE)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted)
                    .child("This surface is no longer here"),
            );
        }

        if let Some(sid) = pending_sid {
            context_menu = context_menu.child(self.render_select_menu_item(
                SharedString::from("tab-cancel-queued"),
                "Cancel queued prompt",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab_menu_open = None;
                    this.cancel_pending_for(sid, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            ));
        }

        context_menu = context_menu.child(
            div()
                .mx(tok::space::XS)
                .my(tok::space::XS)
                .flex_none()
                .h(px(1.))
                .bg(menu_divider_color(ui)),
        );

        // Deleting the surface, not just the pane it is in. The rail row had
        // the only door to it, so a person working in the panes had to go and
        // find the row for a surface they were looking at - and "close" left a
        // row behind, which reads as the app having ignored half the gesture.
        // It is the rail's own path, confirmation and all: one act, one
        // question, wherever it is asked from.
        if let Some(thread_id) = delete_thread_id {
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-delete-surface".into(),
                "Delete session",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab_menu_open = None;
                    if let Some(target) = crate::project::find_surface(&this.workspaces, thread_id)
                    {
                        this.request_delete_for_target(target, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            ));
        }

        // The collapse the `Split` button used to hide. It lives here
        // because the command needs a target, and a menu opened from a pane
        // header has already named one by being where it is - "to *this* pane".
        // Only with something to collapse: on one pane it would name a state
        // the panes are already in.
        if more_than_one_pane {
            let source_for_collapse = source.clone();
            context_menu = context_menu.child(self.render_select_menu_item(
                "tab-context-collapse".into(),
                "Collapse to this pane",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.tab_menu_open = None;
                    this.collapse_to_pane(&source_for_collapse, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            ));
        }

        let source_for_close = source.clone();
        context_menu = context_menu.child(self.render_select_menu_item(
            "tab-context-close".into(),
            "Close pane",
            None,
            ui,
            // The same close as the header's `\u{d7}`, undo record and all.
            // It used to go through `close_tab_at`, which drops the pane by a
            // different route - one that records no undo - so the two doors to
            // one action disagreed about whether it could be taken back.
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.tab_menu_open = None;
                if let Some(ws_idx) = this
                    .workspaces
                    .iter()
                    .position(|ws| ws.contains_pane(&source_for_close))
                {
                    this.record_closed_pane(&source_for_close, ws_idx, cx);
                    this.drop_pane_from_layout(&source_for_close, cx);
                }
                this.save_session(cx);
                cx.stop_propagation();
                cx.notify();
            }),
        ));

        deferred(context_menu).priority(3).into_any_element()
    }

    fn render_disabled_select_menu_item(
        id: SharedString,
        label: &str,
        ui: crate::theme::UiColors,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex_none()
            .h(crate::app::constants::LIST_ROW_HEIGHT)
            .px(tok::space::MD)
            .rounded(tok::radius::CONTROL)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .text_size(tok::text::ROW)
            .text_color(ui.muted)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label.to_string()),
            )
    }

    fn tab_context_path(tab: &TabContent, cx: &App) -> Option<PathBuf> {
        match tab {
            TabContent::Terminal(terminal) => terminal
                .read(cx)
                .terminal
                .current_cwd
                .as_ref()
                .filter(|cwd| !cwd.is_empty())
                .map(PathBuf::from),
            TabContent::Markdown(markdown) => Some(markdown.read(cx).path.clone()),
            TabContent::Diff(diff) => diff.read(cx).column_paths().into_iter().next(),
        }
    }
}
