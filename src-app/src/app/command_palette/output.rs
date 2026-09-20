//! The palette's Output tab: one query across the live output of every open
//! session.
//!
//! This is fleet search, which the design dissolves into the palette rather
//! than giving a screen of its own ("Fleet search (palette, third tab
//! 'Output')"). The fan-out is the one `fleet_search.rs` already argues for: a
//! sequential pass on the background executor, because the per-pane grid locks
//! are disjoint and each is held only for its own scan.
//!
//! **The rows carry counts, not matched text.** The design draws the matching
//! line. The existing memory contract refuses to move match vectors out of the
//! terminals, and a grid scan returns positions rather than text, so lifting
//! the line would mean a second read of every hit. What a row states instead is
//! the session, its project and how many times the query occurs there, and
//! selecting it does exactly what the design says: focuses that session and
//! opens its find bar on the same query, where the matches are.

use gpui::{Context, Window};

use crate::SplitlaneApp;
use crate::app::ipc_handler::find_pane_by_surface_id;
use crate::app::workspace_ops::WorkspaceFocusTarget;

use super::{PaletteItem, PaletteTab};

/// How long the per-tab match-count badges linger after a scan (they
/// auto-dismiss after 4 s or when the search closes).
const BADGE_HOLD_SECS: u64 = 4;

/// One session whose live output matches the query.
pub(crate) struct OutputRow {
    pub(crate) surface_id: u64,
    /// Display name - custom name or OSC title, bidi-stripped and clamped at
    /// collection time (terminal titles are UNTRUSTED).
    pub(crate) name: String,
    pub(crate) project: String,
    pub(crate) count: usize,
}

impl SplitlaneApp {
    /// Open the palette on the Output tab with `query` already in the field.
    ///
    /// The find bar's "Fleet" control lands here. It used to open an overlay of
    /// its own; the design dissolves fleet search into this tab, so there is
    /// one implementation of "search every session" and one place to look for
    /// it.
    pub(crate) fn open_palette_output(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_none() {
            self.open_command_palette(window, cx);
        }
        self.command_palette_query
            .update(cx, |input, cx| input.set_value(&query, cx));
        let Some(state) = &mut self.command_palette else {
            return;
        };
        state.tab = PaletteTab::Output;
        state.selected = 0;
        self.run_palette_output_scan(cx);
    }

    /// Scan every open session's output for the current query.
    ///
    /// An empty query scans nothing: the design's empty state for this tab is
    /// an instruction ("Type to search the live output of every open
    /// session."), not a list of everything.
    pub(crate) fn run_palette_output_scan(&mut self, cx: &mut Context<Self>) {
        let query = self.command_palette_query.read(cx).value().to_string();
        let Some(state) = &mut self.command_palette else {
            return;
        };
        state.output_generation += 1;
        let generation = state.output_generation;
        state.output_query = query.clone();
        if query.is_empty() {
            state.output = Vec::new();
            state.output_running = false;
            cx.notify();
            return;
        }
        state.output_running = true;
        cx.notify();

        // Snapshot the fleet on the main thread with an opaque backend handle;
        // the background scan never receives a concrete terminal-grid type.
        let mut targets: Vec<(u64, String, String, crate::terminal::TerminalSessionBackend)> =
            Vec::new();
        for ws in &self.workspaces {
            let Some(root) = &ws.root else { continue };
            for pane in root.collect_leaves() {
                for view in pane.read(cx).terminals() {
                    let read = view.read(cx);
                    let raw = read
                        .terminal
                        .custom_name
                        .clone()
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| read.terminal.title.clone());
                    let name =
                        crate::markdown::strip_bidi_zero_width(raw.chars().take(64).collect());
                    targets.push((
                        view.entity_id().as_u64(),
                        name,
                        ws.title.clone(),
                        read.terminal.session_backend(),
                    ));
                }
            }
        }

        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let rows = smol::unblock(move || {
                    let mut rows: Vec<OutputRow> = Vec::new();
                    for (surface_id, name, project, backend) in targets {
                        // Plain substring, never regex: this field is the palette's
                        // one query box and also filters names and projects, so a
                        // stray `(` must not turn it into an error.
                        // The palette has one field and no toggles; it counts matches the
                        // way an untoggled find bar would.
                        let result = backend.search(&query, false, false);
                        if !result.matches.is_empty() {
                            rows.push(OutputRow {
                                surface_id,
                                name,
                                project,
                                count: result.matches.len(),
                            });
                        }
                    }
                    rows
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        app.apply_palette_output(generation, rows, cx);
                    })
                });
            },
        )
        .detach();
    }

    fn apply_palette_output(
        &mut self,
        generation: u64,
        rows: Vec<OutputRow>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &mut self.command_palette else {
            return;
        };
        if state.output_generation != generation {
            return;
        }
        let counts: std::collections::HashMap<u64, usize> =
            rows.iter().map(|row| (row.surface_id, row.count)).collect();
        state.output = rows;
        state.output_running = false;
        state.selected = 0;
        cx.notify();

        // The transient per-tab badges (the lowest-priority tab adornment).
        // They came with fleet search and outlive its overlay: a count on the
        // tab is how a match in a session you are not looking at becomes
        // visible at all. Pushed to the LIVE tree, so a pane closed mid-scan
        // never receives one.
        self.push_output_badges(&counts, cx);
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(std::time::Duration::from_secs(BADGE_HOLD_SECS)).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        let current = app
                            .command_palette
                            .as_ref()
                            .map(|state| state.output_generation);
                        if current == Some(generation) {
                            app.push_output_badges(&std::collections::HashMap::new(), cx);
                        }
                    })
                });
            },
        )
        .detach();
    }

    /// Deposit (or clear) the per-tab match counts.
    fn push_output_badges(
        &self,
        counts: &std::collections::HashMap<u64, usize>,
        cx: &mut Context<Self>,
    ) {
        for ws in &self.workspaces {
            let Some(root) = &ws.root else { continue };
            for pane in root.collect_leaves() {
                let subset: std::collections::HashMap<gpui::EntityId, usize> = pane
                    .read(cx)
                    .terminals()
                    .filter_map(|view| {
                        counts
                            .get(&view.entity_id().as_u64())
                            .map(|count| (view.entity_id(), *count))
                    })
                    .collect();
                pane.update(cx, |pane, cx| pane.set_search_hits(subset, cx));
            }
        }
    }

    /// The rows the Output tab shows. Already scoped by the query the scan ran
    /// with - there is nothing further to filter here.
    pub(crate) fn palette_output_items(&self) -> Vec<PaletteItem> {
        let Some(state) = &self.command_palette else {
            return Vec::new();
        };
        (0..state.output.len())
            .map(|index| PaletteItem::Output { index })
            .collect()
    }

    /// Focus the session behind an Output row and arm its find bar with the
    /// same query - the design's own behaviour for this tab.
    pub(crate) fn focus_palette_output_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((surface_id, query)) = self.command_palette.as_ref().and_then(|state| {
            state
                .output
                .get(index)
                .map(|row| (row.surface_id, state.output_query.clone()))
        }) else {
            return;
        };
        let Some((ws_idx, pane, tab_idx)) =
            find_pane_by_surface_id(&self.workspaces, surface_id, cx)
        else {
            // The session closed between render and Enter: drop the row rather
            // than jump somewhere.
            if let Some(state) = &mut self.command_palette {
                state.output.retain(|row| row.surface_id != surface_id);
            }
            cx.notify();
            return;
        };
        self.command_palette = None;
        self.activate_workspace_at(
            ws_idx,
            WorkspaceFocusTarget::PaneTab {
                pane: pane.clone(),
                tab_idx,
            },
            window,
            cx,
        );
        if let Some(view) = pane
            .read(cx)
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.as_terminal())
            .cloned()
        {
            view.update(cx, |view, cx| view.arm_search(&query, false, cx));
        }
        cx.notify();
    }

    /// What the Output tab says when it has nothing to list.
    pub(crate) fn palette_output_empty_message(&self) -> &'static str {
        let Some(state) = &self.command_palette else {
            return "";
        };
        if state.tab != PaletteTab::Output {
            return "";
        }
        if state.output_query.is_empty() {
            "Type to search the live output of every open session."
        } else if state.output_running {
            "Searching every open session…"
        } else {
            "No open session's output matches."
        }
    }
}
