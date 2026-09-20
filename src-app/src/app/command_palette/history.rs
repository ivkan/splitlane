//! The palette's History tab: old sessions, from every agent CLI's own store.
//!
//! "The palette's History tab is the only path to old sessions; there is no
//! separate surface." The rows come from `agent_sessions::read_sessions_for_cwd`,
//! the same reader the docked sessions sidebar uses, run once per open
//! container and off the render thread. That read walks and parses a whole
//! project directory; `CLAUDE.md` says never to do it on the GPUI main thread,
//! and this does not.
//!
//! **Sorted by last activity, never by creation time.** The design is explicit
//! about why: a session created months ago but used daily has to stay at the
//! top, or it is unreachable in a list of 150+. `SessionMeta` already carries
//! `last_activity_secs` for exactly this reason.

use gpui::Context;

use crate::SplitlaneApp;
use crate::agent_sessions::SessionAgent;

use super::{CommandPaletteState, PaletteItem, PaletteTab};

/// How many sessions the tab holds. A palette is a list you scan, not an
/// archive you page through, and the query is the way past the cut.
const HISTORY_CAP: usize = 200;

/// One row of the History tab.
pub(crate) struct HistoryRow {
    pub(crate) agent: SessionAgent,
    pub(crate) session_id: String,
    /// The session's own label - an LLM title, or its first user message.
    pub(crate) title: String,
    /// The opening prompt, as a search key only. When the agent wrote a title
    /// of its own, `title` is that title and this is the text it was made
    /// from - the words the person typed, which is what they are likely to
    /// remember and what the title has replaced.
    pub(crate) first_prompt: Option<String>,
    /// The container this session belongs to, by index and by name.
    pub(crate) ws_idx: usize,
    pub(crate) project: String,
    pub(crate) branch: String,
    pub(crate) last_activity_secs: i64,
}

impl SplitlaneApp {
    /// Read every open container's session store and fill the History tab.
    /// Generation-guarded: a second open while a read is in flight discards
    /// the older answer instead of racing it onto the screen.
    pub(crate) fn load_palette_history(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &mut self.command_palette else {
            return;
        };
        state.history_generation += 1;
        let generation = state.history_generation;

        let agents = crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config);
        let containers: Vec<(usize, String, String)> = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(idx, ws)| (idx, ws.title.clone(), ws.cwd.clone()))
            .collect();

        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let rows = smol::unblock(move || {
                    let mut rows: Vec<HistoryRow> = Vec::new();
                    for (ws_idx, project, cwd) in containers {
                        for agent in &agents {
                            for meta in crate::agent_sessions::read_sessions_for_cwd(*agent, &cwd) {
                                rows.push(HistoryRow {
                                    agent: *agent,
                                    title: meta
                                        .summary
                                        .clone()
                                        .filter(|s| !s.trim().is_empty())
                                        .unwrap_or_else(|| short_id(&meta.session_id)),
                                    first_prompt: meta
                                        .first_prompt
                                        .filter(|s| !s.trim().is_empty()),
                                    session_id: meta.session_id,
                                    ws_idx,
                                    project: project.clone(),
                                    branch: meta.git_branch,
                                    last_activity_secs: meta.last_activity_secs,
                                });
                            }
                        }
                    }
                    // The one ordering rule the design states in bold.
                    rows.sort_by_key(|row| std::cmp::Reverse(row.last_activity_secs));
                    rows.truncate(HISTORY_CAP);
                    rows
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        app.apply_palette_history(generation, rows, cx);
                    })
                });
            },
        )
        .detach();
    }

    fn apply_palette_history(
        &mut self,
        generation: u64,
        rows: Vec<HistoryRow>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &mut self.command_palette else {
            return;
        };
        if state.history_generation != generation {
            return;
        }
        state.history = Some(rows);
        state.selected = 0;
        cx.notify();
    }

    /// The rows the History tab shows for `needle`, filtered on name, project
    /// and branch - the three fields the design names.
    pub(crate) fn palette_history_items(&self, needle: &str) -> Vec<PaletteItem> {
        let Some(state) = &self.command_palette else {
            return Vec::new();
        };
        let Some(rows) = &state.history else {
            return Vec::new();
        };
        rows.iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty()
                    || row.title.to_lowercase().contains(needle)
                    || row
                        .first_prompt
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(needle))
                    || row.project.to_lowercase().contains(needle)
                    || row.branch.to_lowercase().contains(needle)
            })
            .map(|(index, _)| PaletteItem::History { index })
            .collect()
    }

    /// Resume the selected history row into its own container, as a surface of
    /// that container. `SessionBinding::resolve` sees the adopted id already
    /// has a transcript and resumes it on the surface's first mount.
    pub(crate) fn resume_palette_history_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((ws_idx, agent, session_id, title)) = self
            .command_palette
            .as_ref()
            .and_then(|state| state.history.as_ref())
            .and_then(|rows| rows.get(index))
            .map(|row| {
                (
                    row.ws_idx,
                    row.agent.terminal_agent(),
                    row.session_id.clone(),
                    row.title.clone(),
                )
            })
        else {
            return;
        };
        // The palette closes without handing focus back: the surface it just
        // opened is where the user is going.
        self.command_palette = None;
        self.open_session_as_thread_in(ws_idx, agent, &session_id, title, cx);
        cx.notify();
    }
}

/// What a row is called when the session recorded no summary: enough of the id
/// to tell two of them apart, and no more.
fn short_id(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

impl CommandPaletteState {
    /// Whether the History tab is still waiting for its first read.
    pub(crate) fn history_loading(&self) -> bool {
        self.tab == PaletteTab::History && self.history.is_none()
    }
}
