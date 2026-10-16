//! The Running sheet (`⌥⌘P`): everything that is at work, to look over and
//! tidy up.
//!
//! Activity answers "who needs me"; this answers "what is running at all" -
//! before quitting, or when the rail has grown past the screen. It lists the
//! agent sessions with a terminal behind them, every command running in a
//! shell ([`crate::running_commands`]), and the sessions the session host
//! holds with no pane. Idle shells are counted at the bottom and shown on
//! request.
//!
//! It is a sheet in the middle of the window, like the palette: a filter on
//! what is typed, rows grouped by project in the rail's order and never
//! re-sorted, so a row does not move under the pointer as times tick. There
//! is no standing button for it. It is reached by its chord, from the
//! palette and the Window menu, and from the last line of the Activity list
//! when commands are running.
//!
//! **What closing a row does depends on what the row is.** A command is
//! stopped: its process group gets `SIGTERM` (the process itself on
//! Windows), and the pane, its shell and its output stay. An agent session
//! is closed as the rail's Delete closes it, dropping its row; the
//! conversation stays where the agent wrote it. A session with no pane is
//! ended in the session host. One card asks first, naming what will stop.

use std::collections::HashSet;

use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, Styled, Window, deferred, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::project::ThreadStatus;
use crate::running_commands::RunningCommand;
use crate::settings::components::{menu_divider_color, menu_surface};
use crate::ui_tokens as tok;

/// Live sheet state, owned by `SplitlaneApp`.
#[derive(Default)]
pub(crate) struct RunningSheetState {
    /// What has been typed: rows whose text or project contain it.
    query: String,
    /// The keyboard cursor, an index into the visible rows.
    cursor: usize,
    /// Marked rows, by [`SheetRow::key`].
    marked: HashSet<String>,
    /// Idle shells are listed too.
    show_idle: bool,
    /// The rows the close card is asking about, by key.
    confirming: Option<Vec<String>>,
}

/// How a row's dot is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetDot {
    Working,
    Waiting,
    Failed,
    Neutral,
}

/// Where a row leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SheetTarget {
    /// A surface of a project, by thread id.
    Surface(u64),
    /// A session the host holds with no pane, by its id.
    Held(String),
}

/// What closing a row stops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SheetClose {
    /// Signal this command's process group.
    Command { pid: u32 },
    /// Close this surface as the rail's Delete does.
    Surface(u64),
    /// End this held session.
    Held(String),
    /// An idle shell: nothing runs, so closing it closes its surface.
    IdleShell(u64),
}

/// One row of the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SheetRow {
    /// Stable across refreshes, so a mark and the cursor survive them.
    pub(crate) key: String,
    pub(crate) project: String,
    pub(crate) what: String,
    /// The name the close card uses: an agent session's name, or a command's
    /// program and subcommand.
    pub(crate) short: String,
    pub(crate) agent: bool,
    pub(crate) dot: SheetDot,
    /// `no pane`, `stopped`, or nothing.
    pub(crate) tag: Option<&'static str>,
    pub(crate) secs: Option<u64>,
    pub(crate) target: SheetTarget,
    pub(crate) close: SheetClose,
}

/// The rows and the counts the header and footer show.
pub(crate) struct SheetRows {
    pub(crate) rows: Vec<SheetRow>,
    pub(crate) agents: usize,
    pub(crate) commands: usize,
    pub(crate) idle_shells: usize,
}

/// The rows that match `query`, case-insensitively, by text or project.
fn filtered<'a>(rows: &'a [SheetRow], query: &str) -> Vec<&'a SheetRow> {
    let query = query.to_lowercase();
    rows.iter()
        .filter(|row| {
            query.is_empty()
                || row.what.to_lowercase().contains(&query)
                || row.project.to_lowercase().contains(&query)
        })
        .collect()
}

/// `cargo build and npm run dev`, `a, b, c and 2 more`: at most three names,
/// then a count.
pub(crate) fn name_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, c, rest @ ..] => format!("{a}, {b}, {c} and {} more", rest.len()),
    }
}

/// Title, sentence and button of the card that asks before closing `rows`.
pub(crate) fn close_card_copy(rows: &[&SheetRow]) -> (String, String, String) {
    let names: Vec<String> = rows.iter().map(|row| row.short.clone()).collect();
    let (title, button) = match rows {
        [one] => (format!("Close {}?", one.short), "Close".to_string()),
        many => (
            format!("Close {} sessions?", many.len()),
            format!("Close {} sessions", many.len()),
        ),
    };
    let mut body = format!("{} will be stopped.", name_list(&names));
    let commands = rows
        .iter()
        .any(|row| matches!(row.close, SheetClose::Command { .. }));
    let surfaces = rows
        .iter()
        .any(|row| matches!(row.close, SheetClose::Surface(_) | SheetClose::IdleShell(_)));
    if commands {
        body.push_str(" A command's output is kept in scrollback until you close its pane.");
    }
    if surfaces {
        body.push_str(
            " A session's row is dropped; an agent's conversation stays where the agent wrote it.",
        );
    }
    (title, body, button)
}

fn dot_for(status: ThreadStatus) -> SheetDot {
    match status {
        ThreadStatus::Thinking | ThreadStatus::Starting => SheetDot::Working,
        ThreadStatus::WaitingForInput => SheetDot::Waiting,
        ThreadStatus::Failed => SheetDot::Failed,
        ThreadStatus::Idle => SheetDot::Neutral,
    }
}

fn command_row(
    key: String,
    project: &str,
    command: &RunningCommand,
    pane: bool,
    target: SheetTarget,
    now: i64,
) -> SheetRow {
    let tag = if !pane {
        Some("no pane")
    } else if command.stopped {
        Some("stopped")
    } else {
        None
    };
    SheetRow {
        key,
        project: project.to_string(),
        what: command.command.clone(),
        short: crate::running_commands::short_command(&command.command),
        agent: command.agent.is_some(),
        dot: if command.agent.is_some() {
            SheetDot::Working
        } else {
            SheetDot::Neutral
        },
        tag,
        secs: command
            .started_unix
            .and_then(|started| u64::try_from(now - started).ok()),
        target,
        close: SheetClose::Command { pid: command.pid },
    }
}

/// What a held session's project is: the one whose shell it was told it
/// belongs to, if that project is still open.
const NO_PROJECT: &str = "no project";

impl SplitlaneApp {
    /// Every row the sheet can show, in the rail's order.
    pub(crate) fn running_sheet_rows(&self, show_idle: bool) -> SheetRows {
        let now = crate::project::now_unix_secs();
        let window = crate::app::exit_guard::OPEN_SESSION_WINDOW_SECS;
        let fresh = |read: Option<i64>| read.is_some_and(|read| now - read <= window);
        let positions = self.rail_project_positions();
        let mut projects: Vec<usize> = (0..self.workspaces.len()).collect();
        projects.sort_by_key(|idx| positions.get(idx).copied().unwrap_or(usize::MAX));

        let mut out = SheetRows {
            rows: Vec::new(),
            agents: 0,
            commands: 0,
            idle_shells: 0,
        };
        // A held session goes under the project its shell was told it
        // belongs to, after that project's own rows; one whose project is
        // not open goes last.
        let mut held_by_project: std::collections::HashMap<Option<usize>, Vec<SheetRow>> =
            std::collections::HashMap::new();
        if fresh(self.held_commands_read_secs) {
            for held in &self.held_unshown {
                let project = held
                    .exported_workspace
                    .and_then(|id| self.workspaces.iter().position(|ws| ws.id == id));
                let title = project.map_or(NO_PROJECT, |idx| self.workspaces[idx].title.as_str());
                let commands = self.held_commands.get(&held.id);
                if commands.is_none_or(Vec::is_empty) {
                    out.idle_shells += 1;
                    continue;
                }
                for command in commands.into_iter().flatten() {
                    if command.agent.is_some() {
                        out.agents += 1;
                    } else {
                        out.commands += 1;
                    }
                    let mut row = command_row(
                        format!("held:{}:{}", held.id, command.pid),
                        title,
                        command,
                        false,
                        SheetTarget::Held(held.id.clone()),
                        now,
                    );
                    // No pane would be left to show the shell, so the whole
                    // session ends.
                    row.close = SheetClose::Held(held.id.clone());
                    held_by_project.entry(project).or_default().push(row);
                }
            }
        }
        for ws_idx in projects {
            let ws = &self.workspaces[ws_idx];
            let order = crate::app::agents_sidebar::surface_row_order(&ws.threads);
            for &thread_idx in order.agents.iter().chain(order.shells.iter()) {
                let Some(thread) = ws.threads.get(thread_idx) else {
                    continue;
                };
                let shell = crate::app::agents_sidebar::is_shell_surface(thread);
                if crate::app::agents_sidebar::is_agent_session(thread) {
                    let alive = thread
                        .agent_seen_secs
                        .is_some_and(|seen| now - seen <= window);
                    if !alive {
                        continue;
                    }
                    out.agents += 1;
                    let what = match thread.terminal_agent {
                        Some(agent) => format!("{} · {}", thread.title, agent.binary()),
                        None => thread.title.clone(),
                    };
                    let secs = (thread.status == ThreadStatus::Thinking)
                        .then(|| self.running_since.get(&thread.id))
                        .flatten()
                        .map(|started| started.elapsed().as_secs());
                    out.rows.push(SheetRow {
                        key: format!("agent:{}", thread.id),
                        project: ws.title.clone(),
                        what,
                        short: thread.title.clone(),
                        agent: true,
                        dot: dot_for(thread.status),
                        tag: None,
                        secs,
                        target: SheetTarget::Surface(thread.id),
                        close: SheetClose::Surface(thread.id),
                    });
                } else if shell && fresh(thread.commands_read_secs) {
                    if thread.running_commands.is_empty() {
                        out.idle_shells += 1;
                        if show_idle {
                            out.rows.push(SheetRow {
                                key: format!("idle:{}", thread.id),
                                project: ws.title.clone(),
                                what: thread.title.clone(),
                                short: thread.title.clone(),
                                agent: false,
                                dot: SheetDot::Neutral,
                                tag: None,
                                secs: None,
                                target: SheetTarget::Surface(thread.id),
                                close: SheetClose::IdleShell(thread.id),
                            });
                        }
                    }
                    for command in &thread.running_commands {
                        out.commands += 1;
                        out.rows.push(command_row(
                            format!("command:{}:{}", thread.id, command.pid),
                            &ws.title,
                            command,
                            true,
                            SheetTarget::Surface(thread.id),
                            now,
                        ));
                    }
                }
            }
            out.rows
                .extend(held_by_project.remove(&Some(ws_idx)).unwrap_or_default());
        }
        out.rows.extend(held_by_project.into_values().flatten());
        out
    }

    pub(crate) fn handle_open_running_sheet(
        &mut self,
        _: &crate::OpenRunning,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_running_sheet(window, cx);
    }

    pub(crate) fn open_running_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("running sheet: opened");
        self.running_sheet = Some(RunningSheetState::default());
        self.running_sheet_focus.focus(window, cx);
        // What runs is read again now, so the sheet does not open on a
        // reading up to two seconds old.
        self.refresh_running_commands(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(crate) fn close_running_sheet(&mut self, cx: &mut Context<Self>) {
        log::info!("running sheet: closed");
        self.running_sheet = None;
        cx.notify();
        // Leaving from the quit card's Review: the question comes back with
        // the counts as they are now.
        if let Some(intent) = self.running_sheet_return_to_exit.take() {
            self.request_exit(intent, cx);
        }
    }

    fn running_sheet_visible(&self) -> Vec<SheetRow> {
        let Some(state) = &self.running_sheet else {
            return Vec::new();
        };
        let all = self.running_sheet_rows(state.show_idle);
        filtered(&all.rows, &state.query)
            .into_iter()
            .cloned()
            .collect()
    }

    fn running_sheet_go(&mut self, row: &SheetRow, window: &mut Window, cx: &mut Context<Self>) {
        match &row.target {
            SheetTarget::Surface(thread_id) => {
                let Some(target) = crate::project::find_surface(&self.workspaces, *thread_id)
                else {
                    return;
                };
                self.running_sheet = None;
                self.running_sheet_return_to_exit = None;
                self.go_to_stop(
                    crate::app::waiting::WaitingStop::Surface(target),
                    window,
                    cx,
                );
            }
            SheetTarget::Held(id) => {
                let id = id.clone();
                self.running_sheet = None;
                self.running_sheet_return_to_exit = None;
                self.open_held_session(&id, window, cx);
            }
        }
        cx.notify();
    }

    /// Ask before closing the marked rows, or the cursor's row when none is
    /// marked.
    fn running_sheet_ask_close(&mut self, cx: &mut Context<Self>) {
        let visible = self.running_sheet_visible();
        let Some(state) = self.running_sheet.as_mut() else {
            return;
        };
        let keys: Vec<String> = if state.marked.is_empty() {
            visible
                .get(state.cursor)
                .map(|row| row.key.clone())
                .into_iter()
                .collect()
        } else {
            visible
                .iter()
                .filter(|row| state.marked.contains(&row.key))
                .map(|row| row.key.clone())
                .collect()
        };
        if !keys.is_empty() {
            state.confirming = Some(keys);
            cx.notify();
        }
    }

    fn running_sheet_close_confirmed(&mut self, cx: &mut Context<Self>) {
        let Some(keys) = self
            .running_sheet
            .as_mut()
            .and_then(|state| state.confirming.take())
        else {
            return;
        };
        let rows: Vec<SheetRow> = self
            .running_sheet_rows(true)
            .rows
            .into_iter()
            .filter(|row| keys.contains(&row.key))
            .collect();
        for row in &rows {
            match &row.close {
                SheetClose::Command { pid } => {
                    if !crate::running_commands::stop_command(*pid) {
                        log::warn!("running sheet: could not stop pid {pid}");
                    }
                }
                SheetClose::Surface(thread_id) | SheetClose::IdleShell(thread_id) => {
                    if let Some(crate::project::AgentsTarget::Thread { ws_idx, thread_idx }) =
                        crate::project::find_surface(&self.workspaces, *thread_id)
                    {
                        let _ = self.remove_thread(ws_idx, thread_idx, cx);
                    }
                }
                SheetClose::Held(id) => self.end_held_session(id, cx),
            }
        }
        if let Some(state) = self.running_sheet.as_mut() {
            state.marked.retain(|key| !keys.contains(key));
        }
        self.save_session(cx);
        self.refresh_running_commands(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(crate) fn handle_running_sheet_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .running_sheet
            .as_ref()
            .is_some_and(|state| state.confirming.is_some())
        {
            // The card takes its own keys.
            return;
        }
        let visible = self.running_sheet_visible();
        let len = visible.len();
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let Some(state) = self.running_sheet.as_mut() else {
            return;
        };
        state.cursor = state.cursor.min(len.saturating_sub(1));
        match key {
            "escape" => {
                if state.query.is_empty() {
                    self.close_running_sheet(cx);
                } else {
                    state.query.clear();
                    state.cursor = 0;
                }
            }
            "enter" => {
                if let Some(row) = visible.get(state.cursor) {
                    self.running_sheet_go(row, window, cx);
                }
            }
            "up" => state.cursor = state.cursor.saturating_sub(1),
            "down" => {
                if state.cursor + 1 < len {
                    state.cursor += 1;
                }
            }
            "space" => {
                if let Some(row) = visible.get(state.cursor)
                    && !state.marked.remove(&row.key)
                {
                    state.marked.insert(row.key.clone());
                }
            }
            "backspace" if modifiers.secondary() => self.running_sheet_ask_close(cx),
            "backspace" => {
                if state.query.pop().is_some() {
                    state.cursor = 0;
                }
            }
            _ => {
                if let Some(ch) = &event.keystroke.key_char
                    && !ch.is_empty()
                    && !modifiers.control
                    && !modifiers.platform
                    && !modifiers.alt
                {
                    state.query.push_str(ch);
                    state.cursor = 0;
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn render_running_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = crate::theme::ui_colors();
        let Some(state) = &self.running_sheet else {
            return div().into_any_element();
        };
        let all = self.running_sheet_rows(state.show_idle);
        let visible = filtered(&all.rows, &state.query);
        let cursor = state.cursor.min(visible.len().saturating_sub(1));

        let mut counts = Vec::new();
        if all.agents > 0 {
            counts.push(match all.agents {
                1 => "1 agent".to_string(),
                n => format!("{n} agents"),
            });
        }
        if all.commands > 0 {
            counts.push(match all.commands {
                1 => "1 command".to_string(),
                n => format!("{n} commands"),
            });
        }
        let header = div()
            .flex()
            .items_center()
            .gap(tok::space::MD)
            .h(tok::row::TITLE_BAR)
            .px(tok::space::XXL)
            .border_b_1()
            .border_color(menu_divider_color(ui))
            .child(
                div()
                    .text_size(tok::text::ROW)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(ui.text)
                    .child("Running"),
            )
            .child(
                div()
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted)
                    .child(SharedString::from(counts.join(" · "))),
            )
            .child(div().flex_1())
            .child(if state.query.is_empty() {
                div()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(ui.faint)
                    .child(crate::app::running_sheet::OPEN_CHORD_LABEL)
            } else {
                div()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::ROW)
                    .text_color(ui.text)
                    .child(SharedString::from(format!("{}|", state.query)))
            });

        let mut list = div()
            .id("running-sheet-list")
            .flex()
            .flex_col()
            .p(tok::space::SM)
            .max_h(tok::popover::RUNNING_SHEET_LIST_MAX)
            .overflow_y_scroll();
        if visible.is_empty() {
            list = list.child(
                div()
                    .px(tok::space::MD)
                    .py(tok::space::XL)
                    .text_size(tok::text::ROW)
                    .text_color(ui.muted)
                    .child(if all.rows.is_empty() {
                        "Nothing is running"
                    } else {
                        "Nothing matches"
                    }),
            );
        }
        let mut last_project: Option<&str> = None;
        for (index, row) in visible.iter().enumerate() {
            if last_project != Some(row.project.as_str()) {
                last_project = Some(row.project.as_str());
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .h(tok::row::CAPTION)
                        .px(tok::space::MD)
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::PATH)
                        .text_color(ui.faint)
                        .child(SharedString::from(row.project.clone())),
                );
            }
            list = list.child(self.running_sheet_row(row, index == cursor, state, ui, cx));
        }

        let marked = visible
            .iter()
            .filter(|row| state.marked.contains(&row.key))
            .count();
        let footer = div()
            .flex()
            .items_center()
            .gap(tok::space::LG)
            .px(tok::space::XXL)
            .py(tok::space::MD)
            .border_t_1()
            .border_color(menu_divider_color(ui))
            .text_size(tok::text::CAPTION)
            .text_color(ui.faint)
            .when(all.idle_shells > 0 || state.show_idle, |footer| {
                let (sentence, link) = if state.show_idle {
                    ("Idle shells shown · ".to_string(), "Hide")
                } else {
                    let shells = if all.idle_shells == 1 {
                        "shell"
                    } else {
                        "shells"
                    };
                    (
                        format!("{} idle {shells} not shown · ", all.idle_shells),
                        "Show",
                    )
                };
                footer.child(
                    div().flex().child(sentence).child(
                        div()
                            .id("running-sheet-idle-toggle")
                            .text_color(ui.text_secondary)
                            .underline()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(state) = this.running_sheet.as_mut() {
                                    state.show_idle = !state.show_idle;
                                    state.cursor = 0;
                                }
                                cx.notify();
                            }))
                            .child(link),
                    ),
                )
            })
            .child(div().flex_1())
            .when(marked > 0, |footer| {
                footer
                    .child(
                        div()
                            .text_color(ui.muted)
                            .child(SharedString::from(format!("{marked} selected"))),
                    )
                    .child(
                        div()
                            .id("running-sheet-close-marked")
                            .flex()
                            .items_center()
                            .gap(tok::space::SM)
                            .h(tok::row::CAPTION)
                            .px(tok::space::MD)
                            .rounded(tok::radius::TAG)
                            .border_1()
                            .border_color(menu_divider_color(ui))
                            .text_color(ui.text_secondary)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.running_sheet_ask_close(cx);
                            }))
                            .child(SharedString::from(if marked == 1 {
                                "Close 1 session".to_string()
                            } else {
                                format!("Close {marked} sessions")
                            }))
                            .child(
                                div()
                                    .font_family(tok::font::MONO)
                                    .text_size(tok::mono::LABEL)
                                    .text_color(ui.faint)
                                    .child(CLOSE_CHORD_LABEL),
                            ),
                    )
            });

        let sheet = deferred(
            div()
                .id("running-sheet-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(96.))
                .bg(ui.scrim)
                .child(
                    menu_surface(div().id("running-sheet"), ui)
                        .occlude()
                        .track_focus(&self.running_sheet_focus)
                        .on_key_down(cx.listener(Self::handle_running_sheet_key_down))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            if this
                                .running_sheet
                                .as_ref()
                                .is_some_and(|state| state.confirming.is_none())
                            {
                                this.close_running_sheet(cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(tok::popover::RUNNING_SHEET_WIDTH)
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(header)
                        .child(list)
                        // Only when it has something to say: an empty band
                        // under the list reads as a missing control.
                        .when(
                            all.idle_shells > 0 || state.show_idle || marked > 0,
                            |sheet| sheet.child(footer),
                        ),
                ),
        )
        // Under the close card while it asks: the card is drawn at the
        // priority every confirmation uses, below a sheet's.
        .with_priority(if state.confirming.is_some() { 3 } else { 6 });

        let Some(keys) = &state.confirming else {
            return sheet.into_any_element();
        };
        let asked: Vec<SheetRow> = self
            .running_sheet_rows(true)
            .rows
            .into_iter()
            .filter(|row| keys.contains(&row.key))
            .collect();
        let (title, body, confirm_label) = close_card_copy(&asked.iter().collect::<Vec<_>>());
        div()
            .size_full()
            .absolute()
            .top_0()
            .left_0()
            .child(sheet)
            .child(crate::app::confirm_dialog::render_confirm_dialog(
                crate::app::confirm_dialog::ConfirmDialog {
                    id: "running-close",
                    title,
                    body,
                    confirm_label,
                    safe: None,
                    focus: Some(self.running_sheet_focus.clone()),
                    on_cancel: std::rc::Rc::new(|this, _window, cx| {
                        if let Some(state) = this.running_sheet.as_mut() {
                            state.confirming = None;
                        }
                        cx.notify();
                    }),
                    on_confirm: std::rc::Rc::new(|this, _window, cx| {
                        this.running_sheet_close_confirmed(cx);
                    }),
                },
                ui,
                cx,
            ))
            .into_any_element()
    }

    fn running_sheet_row(
        &self,
        row: &SheetRow,
        at_cursor: bool,
        state: &RunningSheetState,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let marked = state.marked.contains(&row.key);
        let dot = match row.dot {
            SheetDot::Working => ui.accent,
            SheetDot::Waiting => ui.attention,
            SheetDot::Failed => ui.agent_error,
            SheetDot::Neutral => ui.faint,
        };
        let go = row.clone();
        let mark_key = row.key.clone();
        div()
            .id(SharedString::from(format!("running-row-{}", row.key)))
            .flex()
            .items_center()
            .gap(tok::space::MD)
            .h(tok::row::PROJECT)
            .px(tok::space::MD)
            .rounded(tok::radius::TAG)
            .when(at_cursor, |el| el.bg(ui.subtle))
            .font_family(tok::font::MONO)
            .text_size(tok::mono::INLINE)
            .cursor(CursorStyle::PointingHand)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.running_sheet_go(&go, window, cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .id(SharedString::from(format!("running-mark-{}", row.key)))
                    .flex_none()
                    .size(px(12.))
                    .rounded(tok::radius::METER)
                    .border_1()
                    .border_color(if marked {
                        ui.text_secondary
                    } else {
                        menu_divider_color(ui)
                    })
                    .when(marked, |el| el.bg(ui.text_secondary))
                    .cursor(CursorStyle::PointingHand)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(state) = this.running_sheet.as_mut()
                            && !state.marked.remove(&mark_key)
                        {
                            state.marked.insert(mark_key.clone());
                        }
                        cx.stop_propagation();
                        cx.notify();
                    })),
            )
            .child(div().flex_none().size(px(6.)).rounded_full().bg(dot))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if row.agent {
                        ui.text
                    } else {
                        ui.text_secondary
                    })
                    .child(SharedString::from(row.what.clone())),
            )
            .when_some(row.tag, |el, tag| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(tok::mono::LABEL)
                        .text_color(ui.muted)
                        .child(tag),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(tok::popover::RUNNING_SHEET_TIME)
                    .flex()
                    .justify_end()
                    .text_size(tok::mono::PATH)
                    .text_color(ui.faint)
                    .child(SharedString::from(
                        row.secs
                            .map(crate::app::attention_queue::wait_label)
                            .unwrap_or_default(),
                    )),
            )
            .into_any_element()
    }
}

/// The sheet's chord as the header and Activity's last line write it.
#[cfg(target_os = "macos")]
pub(crate) const OPEN_CHORD_LABEL: &str = "⌥⌘P";
#[cfg(not(target_os = "macos"))]
pub(crate) const OPEN_CHORD_LABEL: &str = "Ctrl+Alt+P";

#[cfg(target_os = "macos")]
const CLOSE_CHORD_LABEL: &str = "⌘⌫";
#[cfg(not(target_os = "macos"))]
const CLOSE_CHORD_LABEL: &str = "Ctrl+⌫";

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, project: &str, what: &str, close: SheetClose) -> SheetRow {
        SheetRow {
            key: key.to_string(),
            project: project.to_string(),
            what: what.to_string(),
            short: crate::running_commands::short_command(what),
            agent: false,
            dot: SheetDot::Neutral,
            tag: None,
            secs: None,
            target: SheetTarget::Surface(1),
            close,
        }
    }

    #[test]
    fn three_names_then_a_count() {
        let names: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(name_list(&names[..1]), "a");
        assert_eq!(name_list(&names[..2]), "a and b");
        assert_eq!(name_list(&names[..3]), "a, b and c");
        assert_eq!(name_list(&names), "a, b, c and 2 more");
    }

    #[test]
    fn the_filter_reads_the_command_and_the_project() {
        let rows = [
            row(
                "1",
                "atlas",
                "cargo build --release",
                SheetClose::Command { pid: 1 },
            ),
            row(
                "2",
                "landing",
                "npm run dev",
                SheetClose::Command { pid: 2 },
            ),
        ];
        assert_eq!(filtered(&rows, "").len(), 2);
        assert_eq!(filtered(&rows, "CARGO")[0].key, "1");
        assert_eq!(filtered(&rows, "land")[0].key, "2");
        assert!(filtered(&rows, "python").is_empty());
    }

    #[test]
    fn the_close_card_names_what_stops() {
        let build = row(
            "1",
            "atlas",
            "cargo build --release",
            SheetClose::Command { pid: 1 },
        );
        let dev = row(
            "2",
            "landing",
            "npm run dev",
            SheetClose::Command { pid: 2 },
        );
        let (title, body, button) = close_card_copy(&[&build, &dev]);
        assert_eq!(title, "Close 2 sessions?");
        assert_eq!(button, "Close 2 sessions");
        assert!(
            body.starts_with(
                "cargo build and npm run dev will be stopped. A command's output is kept"
            ),
            "{body}"
        );
        let (title, body, button) = close_card_copy(&[&build]);
        assert_eq!(title, "Close cargo build?");
        assert_eq!(button, "Close");
        assert!(body.starts_with("cargo build will be stopped."), "{body}");
        let mut agent = row("3", "atlas", "api · claude", SheetClose::Surface(7));
        agent.short = "api".to_string();
        let (_, body, _) = close_card_copy(&[&agent]);
        assert!(body.contains("row is dropped"), "{body}");
        assert!(!body.contains("scrollback"), "{body}");
    }
}
