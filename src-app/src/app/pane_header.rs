//! What the slot headers are told about the surfaces they are showing.
//!
//! The design describes a pane header as information: the kind glyph,
//! the session name, a badge, the status word, the branch, "↻ rerun" for a
//! shell, the context meter and the close ×. Most of that is a fact about the
//! *surface record* or about the *container* - which the `Pane` cannot see,
//! because a pane never reaches into app state.
//!
//! So the app pushes it, once per frame, exactly as it pushes the agent faces
//! and the diff panes' chrome. Keyed by the surface's terminal entity rather
//! than by its position, because an agent surface moves between slots and
//! panes and its identity travels with the process.

use crate::SplitlaneApp;
use crate::pane::SurfaceFacts;

/// How long after start-up the first model probe runs, and how often it runs
/// after that.
///
/// The badge is a slow fact: a session's model changes when the user changes
/// it, which is rare, and a fresh session has no model at all until its first
/// reply - the same moment the design says the context meter appears. So the
/// probe is paced for a fact that barely moves rather than polled like a
/// status.
const MODEL_PROBE_FIRST_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
const MODEL_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// How far back the spend rate looks.
///
/// Ten minutes is long enough that one expensive turn does not become a trend,
/// and short enough to notice a session that has just started running away. The
/// tail it is measured from is bounded, so on a busy session the window is the
/// limit and on a quiet one the session is - which is why
/// [`crate::agent_state::SpendRate`] carries the span it actually divided by
/// instead of letting a reader assume this number.
const SPEND_WINDOW_SECS: i64 = 10 * 60;

/// The model aliases the badge names a family by. The same three the composer's
/// chip offers, for the same reason: they are what the CLI accepts and echoes
/// back.
const MODEL_FAMILIES: [&str; 3] = ["opus", "sonnet", "haiku"];

/// `claude-opus-4-6-20260101` as `opus 4.6` - the design's badge copy.
///
/// Distinct from the composer's `short_model`, which answers a different
/// question: that chip is where the model is *changed*, so it shows the bare
/// alias `set_model` takes. The badge is where it is *read*, and a header with
/// two agents in it has to say which is on which version.
///
/// Anything that does not parse comes back whole rather than mangled: naming a
/// model wrongly is worse than naming it verbosely.
pub(crate) fn badge_model_label(model: &str) -> String {
    let mut tokens: Vec<&str> = model.split('-').filter(|t| !t.is_empty()).collect();
    // A trailing release date is a build detail, not something to read off a
    // 9.5px badge.
    if tokens
        .last()
        .is_some_and(|t| t.len() == 8 && t.chars().all(|c| c.is_ascii_digit()))
    {
        tokens.pop();
    }
    let Some(family) = tokens
        .iter()
        .copied()
        .find(|t| MODEL_FAMILIES.contains(&t.to_ascii_lowercase().as_str()))
    else {
        return model.to_string();
    };
    let version: Vec<&str> = tokens
        .into_iter()
        .filter(|t| t.chars().all(|c| c.is_ascii_digit()))
        .collect();
    if version.is_empty() {
        family.to_string()
    } else {
        format!("{family} {}", version.join("."))
    }
}

/// How much of a command "↻ rerun: …" spells out before it elides. The design
/// shows the whole command ("↻ rerun: pnpm typecheck"); a real one can be a
/// paragraph, and the header is 34px tall.
const RERUN_LABEL_MAX_CHARS: usize = 32;

/// The command as the rerun affordance names it, and whether the header is
/// showing all of it.
///
/// `false` means the label elides, and the elision is exactly where a second
/// command would hide - so the caller must not submit what the user has not
/// read. Whole when it fits; otherwise the head says which program and the
/// tail says what it was pointed at.
pub(crate) fn rerun_command_label(command: &str) -> (String, bool) {
    let flattened = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flattened.chars().collect();
    if chars.len() <= RERUN_LABEL_MAX_CHARS && flattened == command {
        return (flattened, true);
    }
    if chars.len() <= RERUN_LABEL_MAX_CHARS {
        // Whitespace was normalised away. Nothing is hidden, but what is drawn
        // is no longer byte-for-byte the command, so it does not count as read.
        return (flattened, false);
    }
    let head: String = chars[..RERUN_LABEL_MAX_CHARS / 2].iter().collect();
    let tail: String = chars[chars.len() - (RERUN_LABEL_MAX_CHARS / 2 - 1)..]
        .iter()
        .collect();
    (format!("{head}\u{2026}{tail}"), false)
}

impl SplitlaneApp {
    /// Push what each pane's slot header needs to say about its surfaces.
    /// Mark a surface's row as holding news nobody has looked at.
    ///
    /// Set only where the run ended **out of sight** - a run that finished
    /// under the reader's eyes is not news, and marking it would put a word on
    /// a row they just watched change.
    pub(crate) fn mark_finished_unseen(&mut self, thread_id: u64, cx: &mut gpui::Context<Self>) {
        if let Some(thread) = self.thread_by_id_mut(thread_id)
            && thread.finished_unseen.is_none()
        {
            thread.finished_unseen = Some(crate::project::FinishedMark::unacknowledged());
            cx.notify();
        }
    }

    /// Clear the mark on every surface the active container is showing.
    ///
    /// Once a frame, from `sync_surface_facts`. It walks the panes rather than
    /// the records because the question is "what is on screen", and it reads
    /// the pane's **active tab** - the same rule `pane_kind` and
    /// `FocusedSurface` use, because a pane shows one surface at a time.
    fn clear_unseen_marks_on_screen(&mut self, cx: &mut gpui::Context<Self>) {
        // **The same question the mark asked**, and this half was missing: the
        // mark is set when `surface_is_seen` says no, which is `window_active()
        // && pane_on_screen`, and the clear only asked the second half. So the
        // commonest shape of the whole feature was broken - switch to a
        // browser, a run finishes in a pane that happens to be on screen, and
        // the frame loop wiped the mark before anybody came back to it. A
        // window nobody is looking at is not a window anything was seen in.
        if !crate::agents::notifications::window_active() {
            return;
        }
        let on_screen: Vec<u64> = self
            .active_workspace()
            .and_then(|container| container.root.as_ref())
            .map(|root| {
                root.collect_leaves()
                    .iter()
                    .filter_map(|pane| {
                        let pane = pane.read(cx);
                        pane.tabs
                            .get(pane.selected_idx)
                            .and_then(crate::pane::TabContent::as_terminal)
                            .and_then(|view| view.read(cx).agent_thread_id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if on_screen.is_empty() {
            return;
        }
        let mut cleared = false;
        for thread_id in on_screen {
            if let Some(thread) = self.thread_by_id_mut(thread_id)
                && thread.finished_unseen.take().is_some()
            {
                cleared = true;
            }
        }
        if cleared {
            cx.notify();
        }
    }

    /// Opening Activity **lowers** every mark it lists; it does not clear one.
    ///
    /// An earlier version cleared them all here, under "opening the list is
    /// reading it", and the current one keeps exactly half of that: the list
    /// *has* been seen, so the chip must stop shouting - but a result nobody
    /// has collected is still uncollected, and a popover that emptied the rail would have made the
    /// walk to silence a ritual instead of a reading. So the shout is what the
    /// popover takes, and the quiet mark is what sticks until the surface comes
    /// up in a pane.
    pub(crate) fn acknowledge_unseen_marks(&mut self, cx: &mut gpui::Context<Self>) {
        let mut changed = false;
        for container in &mut self.workspaces {
            for thread in &mut container.threads {
                if let Some(mark) = thread.finished_unseen.as_mut()
                    && !mark.acknowledged
                {
                    mark.acknowledged = true;
                    changed = true;
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Take one surface's mark away, by id.
    ///
    /// The popover's rows are triage: clicking one goes to that session, so it
    /// has been read - **that one**, and pointedly not its neighbours. The
    /// frame loop would get there too once the surface is actually on screen,
    /// and does not always: a click on a row of a *background* container walks
    /// the panes of a container the pass is no longer looking at.
    pub(crate) fn clear_unseen_mark(&mut self, thread_id: u64, cx: &mut gpui::Context<Self>) {
        if let Some(thread) = self.thread_by_id_mut(thread_id)
            && thread.finished_unseen.take().is_some()
        {
            cx.notify();
        }
    }

    /// Surfaces whose run ended unseen, longest ago first, with the mark's own
    /// level.
    ///
    /// Ordered like the waiting rows above them, and for the same reason: the
    /// one that has been standing longest is the one most likely to be the
    /// point of opening the list.
    pub(crate) fn finished_unseen_rows(&self) -> Vec<(u64, crate::project::FinishedMark)> {
        let mut rows: Vec<(u64, crate::project::FinishedMark)> = self
            .workspaces
            .iter()
            .flat_map(|container| {
                container
                    .threads
                    .iter()
                    .filter_map(|thread| Some((thread.id, thread.finished_unseen?)))
            })
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.1.elapsed()));
        rows
    }

    pub(crate) fn sync_surface_facts(&mut self, cx: &mut gpui::Context<Self>) {
        // "Cleared by being read", and this is where reading
        // happens: once a frame, over the panes of the active container, which
        // is exactly the set a person can see. Putting it on the row's click
        // instead would have covered one of the ways a surface comes on screen
        // and missed the palette, the attention chord and a drag onto a pane -
        // several doors, one rule, and the rule belongs where the seeing is.
        self.clear_unseen_marks_on_screen(cx);
        // And then, on the state the clearing left behind, the queue's own
        // edge: a run that ended under the reader's eyes was never marked, so
        // it never enters the queue and never announces. Order matters here and
        // nowhere else in this function.
        self.refresh_attention_edge(cx);
        let Some(root) = self.active_workspace().and_then(|ws| ws.root.as_ref()) else {
            return;
        };
        let ws_idx = self.active_idx;
        // A tooltip must never paint over a menu, and the two are drawn by
        // different layers - GPUI shows a tooltip on hover, the app draws the
        // menu - so nothing else stops the label of the button you just clicked
        // from covering the list that click opened. Every open menu counts, not
        // only a pane's own: a menu is positioned at the pointer and can cover
        // a neighbour.
        let menu_open = self.tab_menu_open.is_some()
            || self.workspace_menu_open.is_some()
            || self.files_menu_open.is_some();
        // One container, one checkout: the branch is the same fact for every
        // surface in these panes. It rides in the per-surface facts all the
        // same, so a surface that one day sits in a worktree of its own needs
        // no second channel to say so.
        let branch = self.workspaces.get(ws_idx).and_then(|container| {
            (!container.git_branch.is_empty()).then(|| {
                (
                    gpui::SharedString::from(container.git_branch.clone()),
                    container.is_worktree,
                )
            })
        });
        let leaves = root.collect_leaves();
        for pane in leaves {
            let mut facts = std::collections::HashMap::new();
            let surfaces: Vec<(gpui::EntityId, Option<u64>)> = pane
                .read(cx)
                .tabs
                .iter()
                .filter_map(|tab| {
                    let view = tab.as_terminal()?;
                    Some((view.entity_id(), view.read(cx).agent_thread_id))
                })
                .collect();
            for (terminal_id, thread_id) in surfaces {
                // A surface record's `terminal_agent` is what separates an
                // agent from a shell: a bare shell created from the rail
                // carries a thread id too, so the id alone answers the wrong
                // question.
                let is_agent = thread_id
                    .and_then(|id| {
                        self.workspaces
                            .get(ws_idx)?
                            .threads
                            .iter()
                            .find(|thread| thread.id == id)
                    })
                    .is_some_and(|thread| thread.terminal_agent.is_some());
                let model = thread_id
                    .and_then(|id| {
                        self.workspaces
                            .get(ws_idx)?
                            .threads
                            .iter()
                            .find(|thread| thread.id == id)
                    })
                    .and_then(|thread| thread.model.as_deref())
                    .map(|model| gpui::SharedString::from(badge_model_label(model)));
                // A shell's record carries a status field too, and it is
                // always `Idle`; reporting it would put a dot and a word on a
                // surface that has no turn to be in.
                let status = thread_id
                    .and_then(|id| {
                        self.workspaces
                            .get(ws_idx)?
                            .threads
                            .iter()
                            .find(|thread| thread.id == id)
                    })
                    .map(|thread| thread.status)
                    .filter(|_| is_agent);
                let context = thread_id
                    .and_then(|id| {
                        self.workspaces
                            .iter()
                            .flat_map(|container| container.threads.iter())
                            .find(|thread| thread.id == id)
                    })
                    .and_then(|thread| {
                        thread
                            .context_tokens
                            .map(|tokens| crate::pane::ContextFact {
                                tokens,
                                ceiling: thread.context_ceiling,
                                compaction: thread.last_compaction,
                                compactions_seen: thread.compactions_seen,
                            })
                    });
                facts.insert(
                    terminal_id,
                    SurfaceFacts {
                        is_agent,
                        model: model.filter(|_| is_agent),
                        status,
                        branch: branch.clone(),
                        context: context.filter(|_| is_agent),
                    },
                );
            }
            pane.update(cx, |pane, cx| {
                pane.set_surface_facts(facts, cx);
                pane.set_menu_open(menu_open, cx);
            });
        }
    }
}

impl SplitlaneApp {
    /// Keep every agent surface's `Thread::model` current from its own
    /// transcript, so the slot header's badge can name it.
    ///
    /// The model is not a fact the app is told: no `ai.*` hook carries it, the
    /// PTY agent never announces it, and `Thread::model` is a field that has
    /// round-tripped through `session.json` for a long time without anything ever
    /// writing it. The transcript on disk is the only source, and it is the
    /// same file the rendered face's own model chip reads - so the two agree by
    /// construction rather than by being kept in step.
    ///
    /// Bounded on both axes: only the surfaces actually showing in the active
    /// container's panes (at most `MAX_PANES` of them), and only the tail of
    /// each transcript.
    pub(crate) fn spawn_model_probe(&self, cx: &mut gpui::Context<Self>) {
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(MODEL_PROBE_FIRST_DELAY).await;
                loop {
                    // Phase 1, main thread: which surfaces are on screen, and
                    // where each one's transcript lives.
                    let targets = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                            app.visible_agent_transcripts(cx)
                        })
                    });
                    let Ok(targets) = targets else {
                        break;
                    };
                    if !targets.is_empty() {
                        // Phase 2, off the render thread: blocking file reads.
                        let read = smol::unblock(move || {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |since| since.as_secs() as i64);
                            targets
                                .into_iter()
                                .filter_map(|(thread_id, path): (u64, std::path::PathBuf)| {
                                    // One read, two facts - see `TailFacts`.
                                    let facts = crate::claude_sessions::read_tail_facts(&path)?;
                                    let spend = crate::agent_state::spend_rate(
                                        &facts.spend,
                                        now,
                                        SPEND_WINDOW_SECS,
                                    );
                                    Some((
                                        thread_id,
                                        facts.model,
                                        spend,
                                        facts.context_tokens,
                                        facts.auto_compacted_at,
                                        facts.compaction,
                                    ))
                                })
                                .collect::<Vec<_>>()
                        })
                        .await;
                        // Phase 3, main thread: record what changed, and only
                        // repaint if something did.
                        let applied = cx.update(|cx| {
                            this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                                let mut changed = false;
                                for (thread_id, model, spend, context, ceiling, compaction) in read
                                {
                                    for container in &mut app.workspaces {
                                        let Some(thread) = container
                                            .threads
                                            .iter_mut()
                                            .find(|thread| thread.id == thread_id)
                                        else {
                                            continue;
                                        };
                                        if let Some(model) = model.as_deref()
                                            && thread.model.as_deref() != Some(model)
                                        {
                                            thread.model = Some(model.to_string());
                                            changed = true;
                                        }
                                        // The rate is stored whatever it is,
                                        // `None` included: a surface that has
                                        // stopped spending must stop reporting
                                        // a rate, and keeping the last one
                                        // would leave a number on screen that
                                        // describes a ten minutes that ended.
                                        // It does not repaint on its own -
                                        // nothing draws it yet, and a probe
                                        // that woke the window twice a minute
                                        // for a number nobody shows would be
                                        // pure cost.
                                        thread.spend = spend;
                                        thread.context_tokens = context;
                                        // Kept once seen: the record that
                                        // carries it leaves the tail as soon as
                                        // the next segment grows, so a pass
                                        // that does not see one is silence
                                        // rather than a denial. A later
                                        // automatic compaction overwrites it,
                                        // which is how a changed plan corrects
                                        // itself.
                                        if ceiling.is_some() {
                                            thread.context_ceiling = ceiling;
                                            changed = true;
                                        }
                                        // Counted by its own timestamp, so a
                                        // pass that sees the same compaction
                                        // again does not count it twice and a
                                        // pass that sees none does not forget
                                        // the last.
                                        if let Some(seen) = compaction {
                                            let is_new = thread
                                                .last_compaction
                                                .is_none_or(|last| last.at != seen.at);
                                            if is_new {
                                                thread.compactions_seen =
                                                    thread.compactions_seen.saturating_add(1);
                                            }
                                            if thread.last_compaction != Some(seen) {
                                                thread.last_compaction = Some(seen);
                                                changed = true;
                                            }
                                        }
                                    }
                                }
                                if changed {
                                    cx.notify();
                                }
                            })
                        });
                        if applied.is_err() {
                            break;
                        }
                    }
                    smol::Timer::after(MODEL_PROBE_INTERVAL).await;
                }
            },
        )
        .detach();
    }

    /// The `(thread id, transcript path)` of every agent surface showing in the
    /// active container's panes.
    fn visible_agent_transcripts(&self, cx: &gpui::App) -> Vec<(u64, std::path::PathBuf)> {
        let Some(root) = self.active_workspace().and_then(|ws| ws.root.as_ref()) else {
            return Vec::new();
        };
        let Some(container) = self.workspaces.get(self.active_idx) else {
            return Vec::new();
        };
        let mut seen = std::collections::HashSet::new();
        let mut targets = Vec::new();
        for pane in root.collect_leaves() {
            for tab in &pane.read(cx).tabs {
                let Some(thread_id) = tab
                    .as_terminal()
                    .and_then(|view| view.read(cx).agent_thread_id)
                else {
                    continue;
                };
                if !seen.insert(thread_id) {
                    continue;
                }
                if let Some(thread) = container.threads.iter().find(|t| t.id == thread_id)
                    && let Some(path) = crate::claude_sessions::transcript_path(thread)
                {
                    targets.push((thread_id, path));
                }
            }
        }
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::badge_model_label;

    #[test]
    fn a_model_id_reads_as_a_family_and_a_version() {
        assert_eq!(badge_model_label("claude-opus-4-6-20260101"), "opus 4.6");
        assert_eq!(badge_model_label("claude-sonnet-5"), "sonnet 5");
        assert_eq!(badge_model_label("claude-3-5-haiku-20241022"), "haiku 3.5");
    }

    #[test]
    fn a_long_command_elides_in_the_middle_and_says_it_is_not_whole() {
        // The head says which program, the tail says what it was pointed at -
        // and `false` is what stops a click submitting what was never read.
        let (label, whole) = super::rerun_command_label(
            "cargo test --workspace --all-targets -- --nocapture pane_header",
        );
        assert!(label.starts_with("cargo test --wo"));
        assert!(label.ends_with("pane_header"));
        assert!(label.chars().count() <= 32);
        assert!(!whole);
    }

    #[test]
    fn a_short_command_is_left_alone_and_counts_as_read() {
        assert_eq!(
            super::rerun_command_label("pnpm typecheck"),
            ("pnpm typecheck".to_string(), true)
        );
    }

    #[test]
    fn collapsing_whitespace_already_means_the_label_is_not_the_command() {
        // Drawn text and command text have diverged, however harmlessly, so
        // the click may not submit it.
        assert_eq!(
            super::rerun_command_label("pnpm  typecheck"),
            ("pnpm typecheck".to_string(), false)
        );
    }

    #[test]
    fn an_unrecognised_id_comes_back_whole() {
        // Naming a model wrongly is worse than naming it verbosely.
        assert_eq!(badge_model_label("gpt-5-codex"), "gpt-5-codex");
    }
}
