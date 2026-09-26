//! Deferred-layer rendering for the Agents-mode context menus and the
//! delete-confirmation dialog's copy (the card is `app::confirm_dialog`).
//!
//! Lives in its own file so [`super::SplitlaneApp::render_agents_sidebar`]
//! does not have to host another 200 lines of menu plumbing. Both
//! renderers reuse [`SplitlaneApp::render_context_menu_item`] +
//! [`SplitlaneApp::shortcut_for_action`] from the workspace
//! context-menu module so the visual language stays identical.

use gpui::{
    AnyElement, ClickEvent, Context, Entity, IntoElement, MouseButton, ParentElement, SharedString,
    Styled, Window, deferred, div, prelude::*, px,
};

use super::state::{AgentsContextMenu, AgentsDeleteTarget, MenuOrigin};
use crate::SplitlaneApp;
use crate::app::sidebar::context_menu::clamped_context_menu_position;
use crate::settings::components::{menu_divider_color, select_menu};
use crate::ui_tokens as tok;

impl SplitlaneApp {
    /// Thread/chat-row right-click menu: Pin/Unpin, Rename, Duplicate,
    /// Restart terminal, Reveal, Delete.
    ///
    /// Parameterized by [`crate::project::AgentsTarget`] so the
    /// project-thread row and the free-chat row share one menu renderer (no
    /// divergent duplicate). The concrete dispatch lives in the
    /// `*_for_target` affordance helpers. Also the title-bar `⋯` overflow
    /// menu reuses this exact renderer.
    pub(crate) fn render_agents_thread_context_menu(
        &self,
        target: crate::project::AgentsTarget,
        position: gpui::Point<gpui::Pixels>,
        origin: MenuOrigin,
        ui: crate::theme::UiColors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Getting the answer out acts on the conversation on screen, not on
        // the surface record, so the design puts it in the panel's overflow
        // menu - and only there. Two further conditions: a shell has no
        // conversation, and an agent whose session file does not exist yet has
        // nothing to copy out of.
        let can_copy = origin == MenuOrigin::SlotHeader
            && self
                .thread_for_target(target)
                .is_some_and(|thread| crate::claude_sessions::transcript_path(thread).is_some());
        // Where that answer can be sent: the other agents on screen beside it.
        // Only where it can be copied, because it is the same answer.
        let send_to: Vec<crate::app::send_answer::AnswerDestination> = if can_copy {
            self.thread_for_target(target)
                .map(|thread| self.answer_destinations(thread.id, cx))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let source_thread_id = self.thread_for_target(target).map(|thread| thread.id);
        // 6 items (Pin, Rename, Duplicate, Restart, Reveal, Delete) +
        // 1 separator + 8px padding => ~225px; one more when the answer can be
        // copied out, and one per pane it can be sent to.
        let menu_height = px(if can_copy { 256. } else { 228. } + send_to.len() as f32 * 28.);
        let menu_pos = clamped_context_menu_position(position, px(220.), menu_height, window);
        let rename_label = "Rename session";
        // The Pin entry's label flips with the target's current
        // state, mirroring the ★/☆ hover toggle.
        let pin_label = if self
            .thread_for_target(target)
            .map(|t| t.pinned)
            .unwrap_or(false)
        {
            "Unpin"
        } else {
            "Pin"
        };

        // Where this surface can be shown: one entry per **other** pane, named
        // by what that pane holds, with its position in the hint column. Plus a
        // new pane when the 320px floor allows one.
        //
        // This is the second door to what a drag onto a pane already
        // did. A drag is unreachable without a mouse and findable nowhere - not
        // by search, not from the keyboard - and in this build the action
        // registry is the floor of reachability, so a drag-only action had no
        // floor at all.
        // Whether it is on screen at all decides more than one thing below, so
        // it is asked once.
        let on_screen = self
            .thread_for_target(target)
            .map(|thread| thread.id)
            .zip(match target {
                crate::project::AgentsTarget::Thread { ws_idx, .. } => Some(ws_idx),
                _ => None,
            })
            .is_some_and(|(thread_id, ws_idx)| self.agent_surface_in_slot(ws_idx, thread_id, cx));
        let destinations: Vec<(Entity<crate::pane::Pane>, String, &'static str)> = self
            .thread_for_target(target)
            .map(|thread| thread.id)
            .and_then(|thread_id| {
                let crate::project::AgentsTarget::Thread { ws_idx, .. } = target else {
                    return None;
                };
                let container = self.workspaces.get(ws_idx)?;
                let root = container.root.as_ref()?;
                let form = root.root_form();
                let leaves = root.collect_leaves();
                let count = leaves.len();
                Some(
                    leaves
                        .into_iter()
                        .enumerate()
                        .filter(|(_, pane)| {
                            crate::app::agent_slots::agent_view_in_pane(pane, thread_id, cx)
                                .is_none()
                        })
                        .map(|(idx, pane)| {
                            let label = pane.read(cx).active_tab_label(cx);
                            (pane, label, Self::pane_slot_label(idx, count, form))
                        })
                        .collect(),
                )
            })
            .unwrap_or_default();
        // A new pane only for a surface that has none. One already on screen
        // has a pane of its own - a pane holds one surface - so "show it in a
        // new pane" would move it across and leave a launcher where it was,
        // which is not what the words promise. Worse, the path that does it
        // does not take the surface out of the old pane, so the session would
        // be in two at once.
        let room_for_new = !on_screen && self.room_for_another_pane_now(cx);

        let mut menu = select_menu("agents-thread-context-menu", ui)
            .occlude()
            .absolute()
            .left(menu_pos.x)
            .top(menu_pos.y)
            .w(px(220.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_agents_menu(cx);
            }))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());

        for (idx, (dest, label, slot)) in destinations.into_iter().enumerate() {
            let hint = (!slot.is_empty()).then(|| SharedString::from(slot));
            menu = menu.child(self.render_select_menu_item(
                SharedString::from(format!("agents-thread-show-in-{idx}")),
                &format!("Show in {label}"),
                hint,
                ui,
                cx.listener(move |this, _: &ClickEvent, w, cx| {
                    this.close_agents_menu(cx);
                    if let crate::project::AgentsTarget::Thread { ws_idx, thread_idx } = target {
                        this.show_surface_in_pane(ws_idx, thread_idx, &dest, w, cx);
                    }
                    cx.stop_propagation();
                }),
            ));
        }
        if room_for_new {
            menu = menu.child(self.render_select_menu_item(
                "agents-thread-show-in-new".into(),
                "Show in a new pane",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, w, cx| {
                    this.close_agents_menu(cx);
                    this.show_surface_in_new_pane(target, w, cx);
                    cx.stop_propagation();
                }),
            ));
        }

        // Pin/Unpin first - the primary "keep this on top" action,
        // consistent with the hover ★. Toggles `thread.pinned` + saves.
        menu = menu.child(self.render_select_menu_item(
            "agents-thread-pin".into(),
            pin_label,
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.close_agents_menu(cx);
                this.toggle_pin_for_target(target, cx);
                cx.stop_propagation();
            }),
        ));

        menu = menu.child(self.render_select_menu_item(
            "agents-thread-rename".into(),
            rename_label,
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, w, cx| {
                this.close_agents_menu(cx);
                this.begin_agents_rename_for_target(target, w, cx);
                cx.stop_propagation();
            }),
        ));

        menu = menu.child(self.render_select_menu_item(
            "agents-thread-duplicate".into(),
            "Duplicate",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.close_agents_menu(cx);
                this.duplicate_agents_target(target, cx);
                cx.stop_propagation();
            }),
        ));

        menu = menu.child(self.render_select_menu_item(
            "agents-thread-restart-terminal".into(),
            "Restart terminal",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.restart_agents_target_terminal(target, cx);
                cx.stop_propagation();
            }),
        ));

        // Reveal the target's cwd (the project dir for a thread, the
        // home dir for a chat). Surfaced both here (right-click) and in the
        // title-bar `⋯` overflow menu (same renderer).
        menu = menu.child(self.render_select_menu_item(
            "agents-thread-reveal".into(),
            "Reveal in File Manager",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.reveal_agents_target(target, cx);
                cx.stop_propagation();
            }),
        ));

        if can_copy {
            // The design's ⌥⌘C, and the main scenario for it: moving a final
            // report into another session without hunting for its first line.
            menu = menu.child(
                self.render_select_menu_item(
                    "agents-thread-copy-last-answer".into(),
                    "Copy last answer",
                    self.effective_shortcuts
                        .iter()
                        .find(|entry| entry.action_name == "copy_last_answer")
                        .map(|entry| {
                            gpui::SharedString::from(crate::keybindings::format_keystroke(
                                &entry.key,
                            ))
                        }),
                    ui,
                    cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.close_agents_menu(cx);
                        this.copy_last_answer_for_agents_target(target, cx);
                        cx.stop_propagation();
                    }),
                ),
            );
        }
        if let Some(source_thread_id) = source_thread_id {
            for (idx, destination) in send_to.into_iter().enumerate() {
                let hint =
                    (!destination.slot.is_empty()).then(|| SharedString::from(destination.slot));
                menu = menu.child(self.render_select_menu_item(
                    SharedString::from(format!("agents-thread-send-answer-{idx}")),
                    &format!("Send last answer to {}", destination.label),
                    hint,
                    ui,
                    cx.listener(move |this, _: &ClickEvent, w, cx| {
                        this.close_agents_menu(cx);
                        this.send_last_answer(source_thread_id, destination.clone(), w, cx);
                        cx.stop_propagation();
                    }),
                ));
            }
        }

        menu = menu.child(
            div()
                .mx(tok::space::XS)
                .my(tok::space::XS)
                .flex_none()
                .h(px(1.))
                .bg(menu_divider_color(ui)),
        );

        menu = menu.child(self.render_select_menu_item(
            "agents-thread-delete".into(),
            "Delete",
            None,
            ui,
            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.request_delete_for_target(target, cx);
                cx.stop_propagation();
            }),
        ));

        deferred(menu).priority(3).into_any_element()
    }

    /// Center-screen confirmation dialog for a pending delete: the copy is
    /// here, the card is [`crate::app::confirm_dialog::render_confirm_dialog`].
    pub(crate) fn render_agents_confirm_delete_dialog(
        &self,
        target: AgentsDeleteTarget,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, body, danger_label) = match target {
            AgentsDeleteTarget::Project { ws_idx } => {
                let (name, thread_count) = self
                    .workspaces
                    .get(ws_idx)
                    .map(|p| (p.title.clone(), p.threads.len()))
                    .unwrap_or_default();
                // Built the same way as the session's, below: name what goes,
                // then say what actually happens. The old copy said "this
                // cannot be undone", which is not true of the part that
                // matters - the folder is untouched and every session's
                // transcript stays where its CLI wrote it. What ends is the
                // processes and the row.
                let body = match thread_count {
                    0 => format!("Delete project \"{name}\"? Splitlane drops its row."),
                    1 => format!(
                        "Delete project \"{name}\"? Splitlane stops its 1 session and drops its row."
                    ),
                    n => format!(
                        "Delete project \"{name}\"? Splitlane stops its {n} sessions and drops its row."
                    ),
                };
                ("Delete project".to_string(), body, "Delete".to_string())
            }
            AgentsDeleteTarget::Thread { thread_id } => {
                let name = self
                    .thread_by_id(thread_id)
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| "this session".to_string());
                (
                    "Delete session".to_string(),
                    // Not "this cannot be undone": the agent CLI writes its own
                    // transcript and we never touch it, so for every agent the
                    // palette's History tab can read, the conversation comes
                    // back. What is actually lost is the running process and
                    // the row - so that is what the sentence says.
                    format!("Delete session \"{name}\"? Splitlane stops it and drops its row."),
                    "Delete".to_string(),
                )
            }
        };
        crate::app::confirm_dialog::render_confirm_dialog(
            crate::app::confirm_dialog::ConfirmDialog {
                id: "agents-confirm",
                title,
                body,
                confirm_label: danger_label,
                focus: None,
                on_cancel: std::rc::Rc::new(|this, _window, cx| {
                    this.cancel_agents_confirm_delete(cx);
                }),
                on_confirm: std::rc::Rc::new(|this, window, cx| {
                    this.execute_agents_confirm_delete(window, cx);
                }),
            },
            ui,
            cx,
        )
    }
}

/// Type-erased view-side helper: given the live `agents_menu_open`,
/// build the right deferred element. Centralised so the main render
/// path is one line.
pub(crate) fn render_open_agents_menu(
    app: &SplitlaneApp,
    menu: AgentsContextMenu,
    ui: crate::theme::UiColors,
    window: &mut Window,
    cx: &mut Context<SplitlaneApp>,
) -> Option<AnyElement> {
    match menu {
        AgentsContextMenu::Thread {
            ws_idx,
            thread_idx,
            position,
            origin,
        } if ws_idx < app.workspaces.len()
            && app
                .workspaces
                .get(ws_idx)
                .map(|p| thread_idx < p.threads.len())
                .unwrap_or(false) =>
        {
            Some(app.render_agents_thread_context_menu(
                crate::project::AgentsTarget::Thread { ws_idx, thread_idx },
                position,
                origin,
                ui,
                window,
                cx,
            ))
        }
        AgentsContextMenu::NewAgent { ws_idx, position } if ws_idx < app.workspaces.len() => {
            Some(app.render_new_agent_menu(ws_idx, position, ui, window, cx))
        }
        AgentsContextMenu::Group { group_id, position }
            if app.project_group(group_id).is_some() =>
        {
            Some(app.render_group_menu(group_id, position, ui, window, cx))
        }
        _ => None,
    }
}
