//! The command palette, on `⌘K`.
//!
//! Three tabs, and the design puts a different question behind each:
//!
//! - **Open sessions** - what is running now, plus the three commands the
//!   design names (New agent session, New shell, Add project).
//! - **History** - old sessions, from every agent CLI's own on-disk store.
//!   "The palette's History tab is the only path to old sessions; there is no
//!   separate surface." Sorted by **last activity, never by creation time**:
//!   a session created months ago but used daily has to stay at the top, or it
//!   is unreachable in a list of 150+.
//! - **Output** - the live output of every open session. This is fleet search,
//!   which the design dissolves into the palette rather than giving it a
//!   screen of its own.
//!
//! The palette is the *floor of reachability*: every action must be findable by name, which is what makes
//! deleting a button a simplification instead of a loss. The design's COMMANDS
//! group is three entries long, which is a fine empty state and not a floor -
//! so typing on the Open tab also matches the action registry, and every action
//! the app has stays one query away. That is a deliberate widening of the
//! design.
//!
//! Two entry points, both required: the chord, and the visible affordance in
//! the title bar / rail / toolbar. The affordance is required precisely
//! because Splitlane draws its own title bar on all three platforms, so a
//! keyboard-only palette would leave a hidden-rail user with no mouse path at
//! all.

mod history;
mod output;
mod render;

use gpui::{Context, FocusHandle, KeyDownEvent, Window};

use crate::SplitlaneApp;

pub(crate) use history::HistoryRow;
pub(crate) use output::OutputRow;

/// Which question the palette is answering.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteTab {
    Open,
    History,
    Output,
}

impl PaletteTab {
    pub(crate) const ALL: [PaletteTab; 3] =
        [PaletteTab::Open, PaletteTab::History, PaletteTab::Output];

    pub(crate) fn label(self) -> &'static str {
        match self {
            PaletteTab::Open => "Open sessions",
            PaletteTab::History => "History",
            PaletteTab::Output => "Output",
        }
    }
}

/// One activatable row. Built fresh on every render from the registry, the
/// live surface list and the two loaded result sets - the palette snapshots
/// none of them, so a pane closed while the palette is open simply stops being
/// offered.
pub(crate) enum PaletteItem {
    /// A registry action, activated by dispatching it.
    Action {
        action_name: &'static str,
        label: String,
        /// The action's context group label ("Global", "Terminal", …), or
        /// "COMMANDS" for the three the design names by hand.
        group: String,
        /// Rendered keystroke, or "Unassigned".
        key: String,
    },
    /// A mounted terminal surface, activated by focusing it.
    Surface {
        surface_id: u64,
        label: String,
        /// Container title, plus the cwd when there is one.
        detail: String,
        /// The design's trailing hint: "agent · restorable" / "shell".
        hint: String,
        /// The project's group, as typed, drawn dim right of the project.
        /// Groups get no rows of their own: a query that names one lists its
        /// projects' sessions. Private groups are no exception - the query
        /// was typed by the person, which is looking.
        group: Option<String>,
    },
    /// A session on disk, activated by resuming it into its project.
    History { index: usize },
    /// A session whose live output matches the query, activated by focusing it
    /// and arming its find bar with the same query.
    Output { index: usize },
}

/// Overlay state (`SplitlaneApp::command_palette`, `None` = closed). The query
/// field itself is a long-lived `TextInput` on `SplitlaneApp`, like every other
/// filter field in the app.
pub(crate) struct CommandPaletteState {
    pub(crate) selected: usize,
    pub(crate) tab: PaletteTab,
    /// The focus to hand back on close. An action carrying a `Terminal`
    /// context has to land on the terminal that was focused when the palette
    /// opened, so the palette restores focus BEFORE dispatching - GPUI's
    /// `Window::dispatch_action` resolves its target from the current focus.
    pub(crate) restore_focus: Option<FocusHandle>,
    /// History rows, `None` until the first read off the render thread lands.
    /// Reading an agent CLI's session store walks and parses a whole project
    /// directory, which is why it never happens on this thread.
    pub(crate) history: Option<Vec<HistoryRow>>,
    pub(crate) history_generation: u64,
    /// Output rows, and the query the fan-out that produced them ran with -
    /// so a stale result set is never shown against a newer query.
    pub(crate) output: Vec<OutputRow>,
    pub(crate) output_query: String,
    pub(crate) output_running: bool,
    pub(crate) output_generation: u64,
}

impl SplitlaneApp {
    /// The chord and the three visible affordances all land here. Toggles: a
    /// second press closes the palette rather than re-opening it.
    pub(crate) fn handle_open_command_palette(
        &mut self,
        _: &crate::OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
        } else {
            self.open_command_palette(window, cx);
        }
    }

    pub(crate) fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.command_palette_query.clone();
        query.update(cx, |input, cx| input.clear(cx));
        self.command_palette = Some(CommandPaletteState {
            selected: 0,
            tab: PaletteTab::Open,
            restore_focus: window.focused(cx),
            history: None,
            history_generation: 0,
            output: Vec::new(),
            output_query: String::new(),
            output_running: false,
            output_generation: 0,
        });
        query.read(cx).focus_handle.clone().focus(window, cx);
        cx.notify();
    }

    pub(crate) fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.command_palette.take() else {
            return;
        };
        if let Some(handle) = state.restore_focus {
            handle.focus(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn command_palette_tab(&self) -> PaletteTab {
        self.command_palette
            .as_ref()
            .map(|state| state.tab)
            .unwrap_or(PaletteTab::Open)
    }

    pub(crate) fn set_command_palette_tab(&mut self, tab: PaletteTab, cx: &mut Context<Self>) {
        let Some(state) = &mut self.command_palette else {
            return;
        };
        if state.tab == tab {
            return;
        }
        state.tab = tab;
        state.selected = 0;
        cx.notify();
        // Each tab pays for its own data, and only once it is looked at: the
        // history read walks the filesystem and the output scan locks every
        // terminal grid in the window.
        match tab {
            PaletteTab::History => self.load_palette_history(cx),
            PaletteTab::Output => self.run_palette_output_scan(cx),
            PaletteTab::Open => {}
        }
    }

    pub(crate) fn command_palette_query_text(&self, cx: &gpui::App) -> String {
        self.command_palette_query.read(cx).value().to_lowercase()
    }

    /// The last row `\u{2193}` may land on.
    ///
    /// The general rule, which replaced a narrower earlier wording after it
    /// turned out to collide with the rule that every action stays reachable:
    ///
    /// > A row the current query selected is in the arrow run. A block that
    /// > stands in the list regardless of the query is not; it is reached by
    /// > click or by its own shortcut.
    ///
    /// So: with an empty query the three COMMANDS rows stand regardless and
    /// are out of the run. With a query typed this palette matches the whole
    /// action registry, and those actions *are* what the query selected - and
    /// this list is the floor of reachability, so putting them out
    /// of arrow reach would make the keyboard worse at the exact moment the
    /// user proved they were using it.
    ///
    /// The design's own palette has an unconditional block, which is the
    /// degenerate case of the same rule rather than a different one.
    fn last_arrow_row(&self, len: usize, cx: &gpui::App) -> usize {
        let last = len.saturating_sub(1);
        if !self.command_palette_query_text(cx).is_empty() {
            return last;
        }
        let items = self.command_palette_items(cx);
        items
            .iter()
            .rposition(|item| !matches!(item, PaletteItem::Action { .. }))
            .unwrap_or(last)
            .min(last)
    }

    /// Build the visible rows for the current tab and query.
    pub(crate) fn command_palette_items(&self, cx: &gpui::App) -> Vec<PaletteItem> {
        let needle = self.command_palette_query_text(cx);
        match self.command_palette_tab() {
            PaletteTab::Open => self.palette_open_items(&needle, cx),
            PaletteTab::History => self.palette_history_items(&needle),
            PaletteTab::Output => self.palette_output_items(),
        }
    }

    /// The Open tab: the sessions that are running, then the actions that
    /// match. With an empty query the action half is the design's three
    /// commands; with a query it is the whole registry, because the
    /// palette is the floor of reachability and three entries are not a floor.
    fn palette_open_items(&self, needle: &str, cx: &gpui::App) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = Vec::new();

        for meta in self.collect_surface_meta(cx) {
            let container = meta.workspace.and_then(|idx| self.workspaces.get(idx));
            let detail = match (container, meta.cwd.as_deref()) {
                (Some(ws), Some(cwd)) => format!("{} · {cwd}", ws.title),
                (Some(ws), None) => ws.title.clone(),
                (None, Some(cwd)) => format!("{} · {cwd}", scope_label(meta.scope)),
                (None, None) => scope_label(meta.scope).to_string(),
            };
            let group = meta
                .workspace
                .and_then(|idx| self.group_of_project(idx))
                .map(|group| group.name.clone());
            // Surface names and titles are OSC-driven and therefore untrusted;
            // `collect_surface_meta` has already scrubbed and clamped them.
            let matches = needle.is_empty()
                || meta.name.to_lowercase().contains(needle)
                || meta.title.to_lowercase().contains(needle)
                || detail.to_lowercase().contains(needle)
                || group
                    .as_deref()
                    .is_some_and(|group| group.to_lowercase().contains(needle));
            if !matches {
                continue;
            }
            let hint = self.surface_hint(&meta, cx);
            items.push(PaletteItem::Surface {
                surface_id: meta.surface_id,
                label: meta.name.clone(),
                detail,
                hint,
                group,
            });
        }

        if needle.is_empty() {
            for action_name in DESIGN_COMMANDS {
                if let Some(item) = self.palette_action_item(action_name, "COMMANDS") {
                    items.push(item);
                }
            }
            return items;
        }

        for (group, rows) in crate::keybindings::group_shortcuts(&self.effective_shortcuts, needle)
        {
            for (_, entry) in rows {
                items.push(PaletteItem::Action {
                    action_name: entry.action_name,
                    label: entry.description.clone(),
                    group: group.to_string(),
                    key: entry.key.clone(),
                });
            }
        }
        items
    }

    fn palette_action_item(&self, action_name: &'static str, group: &str) -> Option<PaletteItem> {
        let entry = self
            .effective_shortcuts
            .iter()
            .find(|entry| entry.action_name == action_name)?;
        Some(PaletteItem::Action {
            action_name: entry.action_name,
            label: entry.description.clone(),
            group: group.to_string(),
            key: entry.key.clone(),
        })
    }

    /// The design's trailing hint on an open row: "agent · restorable" for an
    /// agent surface, "shell" for everything else. Restorable is not
    /// decoration; it is whether the surface comes back on the next launch, and
    /// only a surface carrying a session binding does.
    fn surface_hint(&self, meta: &crate::app::ipc_handler::SurfaceMeta, cx: &gpui::App) -> String {
        let Some(ws) = meta.workspace.and_then(|idx| self.workspaces.get(idx)) else {
            return "shell".to_string();
        };
        let is_agent = ws.root.as_ref().is_some_and(|root| {
            root.collect_leaves().iter().any(|pane| {
                pane.read(cx).terminals().any(|view| {
                    view.entity_id().as_u64() == meta.surface_id
                        && view.read(cx).agent_thread_id.is_some()
                })
            })
        });
        if is_agent {
            "agent · restorable".to_string()
        } else {
            "shell".to_string()
        }
    }

    fn activate_command_palette_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        enum Target {
            Action(Option<Box<dyn gpui::Action>>),
            Surface(u64),
            History(usize),
            Output(usize),
        }
        let target = match self.command_palette_items(cx).get(index) {
            Some(PaletteItem::Action { action_name, .. }) => {
                Target::Action(crate::keybindings::action_from_name(action_name))
            }
            Some(PaletteItem::Surface { surface_id, .. }) => Target::Surface(*surface_id),
            Some(PaletteItem::History { index }) => Target::History(*index),
            Some(PaletteItem::Output { index }) => Target::Output(*index),
            None => return,
        };
        match target {
            Target::Action(Some(action)) => {
                // Close first, dispatch second: the palette's own field holds
                // focus right now, and a `Terminal`-context handler lives on
                // the terminal's dispatch path, not on the palette's.
                self.close_command_palette(window, cx);
                window.dispatch_action(action, cx);
            }
            // Registered but unresolvable is impossible today - the registry
            // is both the source of the row and of the factory - so drop the
            // row rather than leave the palette open on a dead press.
            Target::Action(None) => self.close_command_palette(window, cx),
            Target::Surface(surface_id) => {
                // Focusing the surface supersedes the restore target, so drop
                // the state without handing focus back to where we came from.
                self.command_palette = None;
                if self.focus_surface_by_id(surface_id, window, cx) {
                    // Its group opens if it was folded, private or not.
                    self.reveal_project_group(self.active_idx, cx);
                }
                cx.notify();
            }
            Target::History(index) => self.resume_palette_history_row(index, cx),
            Target::Output(index) => self.focus_palette_output_row(index, window, cx),
        }
    }

    pub(crate) fn handle_command_palette_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_none() {
            return;
        }
        let len = self.command_palette_items(cx).len();
        let selected = self
            .command_palette
            .as_ref()
            .map(|state| state.selected)
            .unwrap_or(0);
        match event.keystroke.key.as_str() {
            "escape" => {
                // Two-stage, like the settings search: a non-empty query is
                // cleared first, so one press never throws away both a long
                // query and the palette.
                if self.command_palette_query.read(cx).value().is_empty() {
                    self.close_command_palette(window, cx);
                } else {
                    self.command_palette_query
                        .update(cx, |input, cx| input.clear(cx));
                    self.set_command_palette_selection(0, cx);
                }
                cx.stop_propagation();
            }
            // The tabs are three questions, and `tab` is the key that moves
            // between them everywhere else in this app.
            "tab" => {
                let current = self.command_palette_tab();
                let idx = PaletteTab::ALL
                    .iter()
                    .position(|tab| *tab == current)
                    .unwrap_or(0);
                let step = if event.keystroke.modifiers.shift {
                    PaletteTab::ALL.len() - 1
                } else {
                    1
                };
                self.set_command_palette_tab(
                    PaletteTab::ALL[(idx + step) % PaletteTab::ALL.len()],
                    cx,
                );
                cx.stop_propagation();
            }
            "enter" if len > 0 => {
                self.activate_command_palette_row(selected.min(len - 1), window, cx);
                cx.stop_propagation();
            }
            "up" if len > 0 => {
                self.set_command_palette_selection(selected.saturating_sub(1), cx);
                cx.stop_propagation();
            }
            "down" if len > 0 => {
                let last = self.last_arrow_row(len, cx);
                self.set_command_palette_selection((selected + 1).min(last), cx);
                cx.stop_propagation();
            }
            _ => {
                // Anything else edits the query, and the old cursor would
                // point into the previous (longer) result set. The Output tab
                // has to go and look again, too - its rows come from a scan of
                // live terminals, not from a filter over something held.
                self.set_command_palette_selection(0, cx);
                if self.command_palette_tab() == PaletteTab::Output {
                    self.run_palette_output_scan(cx);
                }
            }
        }
    }

    pub(crate) fn set_command_palette_selection(
        &mut self,
        selected: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &mut self.command_palette {
            state.selected = selected;
            cx.notify();
        }
    }

    pub(crate) fn activate_command_palette_row_public(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_command_palette_row(index, window, cx);
    }
}

/// The three the design lists under COMMANDS, in its order.
const DESIGN_COMMANDS: [&str; 3] = ["new_agent", "new_shell", "new_workspace"];

/// Human label for a surface with no container title to show.
fn scope_label(scope: &str) -> &'static str {
    match scope {
        "agents_thread" => "Agent surface",
        "agents_bottom" => "Agent terminal",
        _ => "Surface",
    }
}
