//! Sessions the session host holds that no pane shows.
//!
//! A session in the host outlives the window. Most of them are found again
//! by the layout that was saved with them: each pane and each agent session
//! records which session its terminal ran in, and a window that starts
//! attaches to the ones still running. What is left over is a session the
//! saved layout does not mention - its window was killed before the layout
//! was written, or the layout was written by a window that could not see
//! it. Such a session runs on with nobody looking, and for an agent that
//! means spending with nobody looking.
//!
//! So a window that finds any says so once, when it starts, with a card
//! that lists them. Each can be opened as a tab of the pane the person is
//! in, or ended. Putting the card away ends nothing: the sessions stay,
//! and the next window lists them again.
//!
//! The card takes no keyboard focus. It is answered with the pointer, and
//! the panes keep the keys while it is up.

use gpui::{
    AnyElement, AppContext, ClickEvent, Context, Focusable, FontWeight, InteractiveElement,
    IntoElement, MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Window, deferred, div, px,
};
use splitlane_config::schema::{LayoutNode, SessionState};

use crate::SplitlaneApp;
use crate::running_commands::RunningCommand;
use crate::terminal::TerminalView;
use crate::terminal::held_sessions::HeldSession;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// How long a window waits, when it starts, for the host to say what it
/// holds. The answer is one request over a local socket; the bound is for
/// a host that has stopped answering, which must not keep the window from
/// opening.
pub(crate) const LISTING_WAIT: std::time::Duration = std::time::Duration::from_millis(400);

/// The ids of the host sessions a saved session mentions, on a pane's tab
/// or on an agent session's record.
fn mentioned(session: &SessionState) -> std::collections::HashSet<&str> {
    fn walk<'a>(node: &'a LayoutNode, found: &mut std::collections::HashSet<&'a str>) {
        match node {
            LayoutNode::Pane { surfaces } => {
                found.extend(surfaces.iter().filter_map(|s| s.host_session.as_deref()));
            }
            LayoutNode::Split { children, .. } => {
                for child in children {
                    walk(child, found);
                }
            }
        }
    }
    let mut found = std::collections::HashSet::new();
    for project in &session.projects {
        if let Some(layout) = &project.layout {
            walk(layout, &mut found);
        }
        found.extend(
            project
                .surfaces
                .iter()
                .filter_map(|surface| surface.agent.as_ref()?.host_session.as_deref()),
        );
    }
    found
}

/// The held sessions the saved session does not mention, oldest first.
pub(crate) fn unshown(held: &[HeldSession], session: Option<&SessionState>) -> Vec<HeldSession> {
    let mentioned = session.map(mentioned).unwrap_or_default();
    let mut unshown: Vec<HeldSession> = held
        .iter()
        .filter(|held| !mentioned.contains(held.id.as_str()))
        .cloned()
        .collect();
    unshown.sort_by_key(|held| held.created_at_ms);
    unshown
}

/// The command in a held session that names it: the agent when one runs,
/// else the first command the census found.
fn naming_command(commands: &[RunningCommand]) -> Option<&RunningCommand> {
    commands
        .iter()
        .find(|command| command.agent.is_some())
        .or_else(|| commands.first())
}

/// One row of the card: what the session runs, in which folder, since when.
///
/// The folder alone was the whole label once, and several sessions left in
/// one project read as one line repeated: nothing told the agent that was
/// halfway through a task from the shell nobody needed. So the row leads
/// with the agent session's title, or the agent, or the command, or says it
/// is a shell. `commands` is `None` until the first census is in, when what
/// runs is not known yet and only the folder is said.
fn row_label(
    held: &HeldSession,
    commands: Option<&[RunningCommand]>,
    title: Option<&str>,
) -> String {
    let started = chrono::DateTime::from_timestamp_millis(held.created_at_ms as i64)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "an unknown time".to_string());
    let folder = std::path::Path::new(&held.cwd)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| held.cwd.clone());
    let Some(commands) = commands else {
        return format!("{folder} - since {started}");
    };
    let what = match naming_command(commands) {
        None => "Shell".to_string(),
        Some(command) => match (title, command.agent) {
            (Some(title), Some(_)) => title.to_string(),
            (None, Some(binary)) => crate::agent_launcher::TerminalAgent::from_binary(binary)
                .map(|agent| agent.display_name().to_string())
                .unwrap_or_else(|| binary.to_string()),
            _ => crate::running_commands::short_command(&command.command),
        },
    };
    format!("{what} - {folder}, since {started}")
}

impl SplitlaneApp {
    /// Read, once each and off the main thread, the title of the Claude Code
    /// session each held session runs, for the card's rows. Called when a
    /// census brings new commands; a session already read, or being read,
    /// is not read again.
    pub(crate) fn read_held_titles(&mut self, cx: &mut Context<Self>) {
        for held in &self.held_unshown {
            if self.held_titles.contains_key(&held.id) {
                continue;
            }
            let Some(session_id) = self
                .held_commands
                .get(&held.id)
                .and_then(|commands| naming_command(commands))
                .filter(|command| {
                    command.agent == Some(crate::agent_launcher::TerminalAgent::ClaudeCode.binary())
                })
                .and_then(|command| crate::claude_sessions::session_in_command(&command.command))
                .map(str::to_string)
            else {
                continue;
            };
            self.held_titles.insert(held.id.clone(), None);
            let (id, cwd) = (held.id.clone(), held.cwd.clone());
            cx.spawn(async move |this, cx| {
                let title =
                    smol::unblock(move || crate::claude_sessions::session_title(&cwd, &session_id))
                        .await;
                if title.is_some() {
                    let _ = this.update(cx, |this, cx| {
                        this.held_titles.insert(id, title);
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    /// Show a held session in a new tab of the pane the person is in.
    pub(crate) fn open_held_session(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(at) = self.held_unshown.iter().position(|held| held.id == id) else {
            return;
        };
        let Some((container, pane)) = self
            .active_workspace()
            .map(|container| container.id)
            .zip(self.focused_pane_as_shown(window, cx))
        else {
            self.open_held_session_as_pane(at, window, cx);
            return;
        };
        let held = self.held_unshown.remove(at);
        let cwd = std::path::PathBuf::from(&held.cwd);
        let terminal = cx.new(|cx| TerminalView::with_cwd(container, Some(cwd), None, cx));
        terminal.read(cx).attach_to_held_session(held.id, None);
        cx.subscribe(&terminal, Self::handle_terminal_event)
            .detach();
        pane.update(cx, |pane, cx| pane.show_terminal(terminal, cx));
        pane.read(cx).focus_handle(cx).focus(window, cx);
        // Now the layout mentions it, and the next window attaches to it
        // without asking.
        self.save_session(cx);
        cx.notify();
    }

    /// Show a held session when there is no pane to put it in: a window
    /// with no project, or a project with no panes.
    ///
    /// It used to say "Open a project first" and do nothing, and in a window
    /// with no project that left the card's `Open` with no way to work: the
    /// card covers the window, and putting it away hid the sessions until the
    /// next launch. So the session becomes a pane of its own, in the project
    /// its shell was told it belongs to when that one is open, else in the
    /// project on screen, else in a new project at the session's directory.
    fn open_held_session_as_pane(
        &mut self,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(held) = self.held_unshown.get(at) else {
            return;
        };
        let cwd = std::path::PathBuf::from(&held.cwd);
        let own = held
            .exported_workspace
            .and_then(|id| self.workspaces.iter().position(|ws| ws.id == id));
        let on_screen = (!self.workspaces.is_empty()).then_some(self.active_idx);
        let ws_idx = match own.or(on_screen) {
            Some(idx) => idx,
            None => {
                let title = cwd
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| held.cwd.clone());
                if self.create_container(title, cwd.clone(), cx).is_none() {
                    self.show_toast("Too many projects are open to add one for it.", cx);
                    return;
                }
                self.workspaces.len() - 1
            }
        };
        let held = self.held_unshown.remove(at);
        let container = self.workspaces[ws_idx].id;
        let terminal = cx.new(|cx| TerminalView::with_cwd(container, Some(cwd), None, cx));
        terminal.read(cx).attach_to_held_session(held.id, None);
        cx.subscribe(&terminal, Self::handle_terminal_event)
            .detach();
        let pane = self.create_pane(terminal, cx);
        let ws = &mut self.workspaces[ws_idx];
        match ws.root.as_mut() {
            Some(root) => {
                root.split_first_leaf(crate::layout::SplitDirection::Horizontal, pane.clone());
            }
            None => ws.root = Some(crate::layout::LayoutTree::Leaf(pane.clone())),
        }
        self.activate_workspace_at(
            ws_idx,
            crate::app::workspace_ops::WorkspaceFocusTarget::PaneTab { pane, tab_idx: 0 },
            window,
            cx,
        );
    }

    /// End a held session without ever showing it.
    pub(crate) fn end_held_session(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(at) = self.held_unshown.iter().position(|held| held.id == id) else {
            return;
        };
        let held = self.held_unshown.remove(at);
        crate::terminal::held_sessions::end(&held);
        cx.notify();
    }

    /// Put the card away. The sessions stay as they are.
    fn dismiss_held_sessions(&mut self, cx: &mut Context<Self>) {
        self.held_card_dismissed = true;
        cx.notify();
    }

    /// Whether the card is to be drawn.
    pub(crate) fn held_sessions_card_is_up(&self) -> bool {
        !self.held_card_dismissed && !self.held_unshown.is_empty()
    }

    pub(crate) fn render_held_sessions_card(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let resting = ui.subtle;
        let hover = ui.surface;
        let button = |id: SharedString, label: &'static str| {
            div()
                .id(id)
                .px(tok::space::XL)
                .py(tok::space::MD)
                .rounded(tok::radius::SMALL)
                .bg(resting)
                .text_size(tok::text::ROW)
                .font_weight(FontWeight::MEDIUM)
                .text_color(ui.text)
                .animated_hover(move |style, delta| {
                    style.bg(lerp_color(resting, hover, delta));
                })
                .cursor_pointer()
                .child(label)
        };
        let census_in = self.held_commands_read_secs.is_some();
        let rows = self.held_unshown.iter().map(|held| {
            let (open_id, end_id) = (held.id.clone(), held.id.clone());
            let label = row_label(
                held,
                census_in.then(|| {
                    self.held_commands
                        .get(&held.id)
                        .map_or(&[][..], Vec::as_slice)
                }),
                self.held_titles.get(&held.id).and_then(Option::as_deref),
            );
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::MD)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_size(tok::text::ROW)
                        .text_color(ui.text)
                        .child(label),
                )
                .child(
                    button(format!("held-open-{}", held.id).into(), "Open").on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.open_held_session(&open_id, window, cx);
                        },
                    )),
                )
                .child(
                    button(format!("held-end-{}", held.id).into(), "End").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.end_held_session(&end_id, cx);
                        },
                    )),
                )
        });
        let count = self.held_unshown.len();
        // `Not now` puts the card away until the next launch; the sheet is
        // where the sessions can be found before then.
        let chord = crate::app::running_sheet::OPEN_CHORD_LABEL;
        let body = if count == 1 {
            format!(
                "A session kept running after its window was gone, and no pane shows it. \
                 Open it in the pane you are in, or end it. Running ({chord}) lists it \
                 until you do."
            )
        } else {
            format!(
                "{count} sessions kept running after their window was gone, and no pane \
                 shows them. Open each in the pane you are in, or end it. Running \
                 ({chord}) lists them until you do."
            )
        };
        let card = div()
            .id("held-sessions-card")
            .occlude()
            .w(px(520.))
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .rounded(tok::radius::WINDOW)
            .shadow(crate::ui_primitives::menu_shadow(ui))
            .p(tok::space::XXL)
            .flex()
            .flex_col()
            .gap(tok::space::MD)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(tok::text::TITLE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui.text)
                    .child("Sessions still running"),
            )
            .child(
                div()
                    .text_size(tok::text::ROW)
                    .text_color(ui.muted)
                    .child(body),
            )
            .children(rows)
            .child(
                div()
                    .mt(tok::space::XS)
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(button("held-sessions-later".into(), "Not now").on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_held_sessions(cx)),
                    )),
            );
        let backdrop = div()
            .id("held-sessions-backdrop")
            .occlude()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .bg(gpui::black().opacity(0.45))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_held_sessions(cx)),
            )
            .child(card);
        deferred(backdrop).priority(4).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitlane_config::schema::{
        AgentSurface, ProjectSession, ProjectSurface, SurfaceDefinition, SurfaceKind,
        SurfacePlacement,
    };

    fn held(id: &str, created_at_ms: u64) -> HeldSession {
        HeldSession {
            id: id.to_string(),
            pid: 100,
            cwd: "/work".to_string(),
            program: "/bin/zsh".to_string(),
            created_at_ms,
            exported_surface: None,
            exported_workspace: None,
        }
    }

    fn saved(tab: Option<&str>, agent: Option<&str>) -> SessionState {
        let mut session: SessionState =
            serde_json::from_str(r#"{"version":2,"active_workspace":0}"#).expect("a session");
        let mut project: ProjectSession =
            serde_json::from_str(r#"{"id":1,"title":"p","cwd":"/work"}"#).expect("a project");
        let surface: SurfaceDefinition = serde_json::from_value(serde_json::json!({
            "surface_type": "terminal",
            "host_session": tab,
        }))
        .expect("a tab");
        project.layout = Some(LayoutNode::Split {
            direction: "horizontal".to_string(),
            ratio: None,
            ratios: None,
            children: vec![LayoutNode::Pane {
                surfaces: vec![surface],
            }],
        });
        let payload: AgentSurface = serde_json::from_value(serde_json::json!({
            "agent": "claude_code",
            "host_session": agent,
        }))
        .expect("an agent session");
        project.surfaces.push(ProjectSurface {
            id: 2,
            kind: SurfaceKind::Agent,
            placement: SurfacePlacement::Parked,
            title: "a".to_string(),
            cwd: "/work".to_string(),
            created_at: 0,
            agent: Some(payload),
        });
        session.projects.push(project);
        session
    }

    #[test]
    fn a_session_the_saved_layout_mentions_is_not_listed() {
        let all = [held("tab", 3), held("agent", 2), held("left-over", 1)];
        // A tab of a pane inside a split, and an agent session that is in
        // no pane at all: both are the layout's.
        let session = saved(Some("tab"), Some("agent"));
        assert_eq!(unshown(&all, Some(&session)), [held("left-over", 1)]);
    }

    #[test]
    fn with_no_saved_layout_every_held_session_is_listed_oldest_first() {
        let all = [held("b", 20), held("a", 10)];
        assert_eq!(unshown(&all, None), [held("a", 10), held("b", 20)]);
        assert_eq!(
            unshown(&all, Some(&saved(None, None))),
            [held("a", 10), held("b", 20)]
        );
    }

    fn command(line: &str, agent: Option<&'static str>) -> RunningCommand {
        RunningCommand {
            pid: 7,
            command: line.to_string(),
            started_unix: None,
            stopped: false,
            agent,
        }
    }

    #[test]
    fn before_the_census_a_row_says_only_the_folder() {
        assert!(row_label(&held("x", 0), None, None).starts_with("work - since "));
    }

    #[test]
    fn a_row_leads_with_what_the_session_runs() {
        let agent = [
            command("zsh-helper", None),
            command("claude --session-id abc", Some("claude")),
        ];
        let label =
            |commands: &[RunningCommand], title| row_label(&held("x", 0), Some(commands), title);
        assert!(label(&agent, Some("Fix the build")).starts_with("Fix the build - work, since "));
        assert!(label(&agent, None).starts_with("Claude Code - work, since "));
        assert!(
            label(&[command("/usr/bin/npm run dev --port 3", None)], None)
                .starts_with("npm run dev - work, since ")
        );
        // A title is the agent's; a plain command keeps its own name.
        assert!(
            label(&[command("npm run dev", None)], Some("stale"))
                .starts_with("npm run dev - work, since ")
        );
        assert!(label(&[], None).starts_with("Shell - work, since "));
    }
}
