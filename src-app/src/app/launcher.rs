//! The launcher's list: what can go into a pane that holds nothing yet.
//!
//! The design rewrote this surface. It used to be "the choice of
//! CLI agent" - a grid of agent tiles - which answers a narrower question than
//! the one being asked. The question is *what goes here*, and two thirds of
//! the answers were missing: to show something already running you had to
//! leave the pane and go to the rail.
//!
//! So it is **one list in three parts**, filtered by a single field:
//!
//! | Part | Rows |
//! |---|---|
//! | Start something new | one row per agent on PATH, each with its ladder word, then New shell |
//! | Open sessions | this container's surfaces that are not in a pane |
//! | Recent on disk | sessions from this container's own history |
//!
//! Deliberately the same inventory and order as `⌘K`, so the empty pane
//! teaches nothing new - the only difference is where the pick lands.
//!
//! **Agents not on PATH are absent, not dimmed.** The tile grid drew them at
//! 58% opacity, let the click through, and answered with a toast: a row that
//! refuses every click is worse than no row, and a reason you have to provoke
//! is not a reason the row gave.
//!
//! **This container only, in both list parts.** The design's mockup shows
//! sessions from every project, because in it a pane is not owned by one. Here
//! a surface belongs to a directory and a pane belongs to a container, so a row
//! from another project could not fill this pane without running a session
//! somewhere it does not live. Crossing projects is what `⌘K` is for.
//!
//! **The pane builds none of this.** A `Pane` knows a `workspace_id` and not a
//! directory, cannot read a session store, and must not: the rows are pushed
//! once per frame beside the surface facts, and the pick comes back as an
//! event. Same split as `pane_header::sync_surface_facts`.

use gpui::{Context, Entity, SharedString};

use crate::SplitlaneApp;
use crate::agent_launcher::TerminalAgent;
use crate::pane::Pane;

/// Which part of the list a row sits in. Also its order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LauncherGroup {
    New,
    Open,
    Recent,
}

impl LauncherGroup {
    pub(crate) fn label(self) -> &'static str {
        match self {
            LauncherGroup::New => "START SOMETHING NEW",
            LauncherGroup::Open => "OPEN SESSIONS",
            LauncherGroup::Recent => "RECENT ON DISK",
        }
    }
}

/// What picking a row does. Resolved by the app, which is the only thing that
/// knows what a container is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LauncherAction {
    /// Start a session with this CLI in the pane's container.
    NewAgent(TerminalAgent),
    /// A plain shell in the container's directory.
    NewShell,
    /// A surface this container already has, currently in no pane.
    ShowSurface { thread_id: u64 },
    /// A session on disk, resumed into this container.
    ResumeSession {
        agent: TerminalAgent,
        session_id: String,
        title: String,
    },
}

/// One row, built by the app and read by the pane.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LauncherRow {
    pub(crate) group: LauncherGroup,
    /// `◆` an agent, `▷` a shell, `◇` something on disk - the rail's glyphs,
    /// so a row means the same thing in both places.
    pub(crate) glyph: SharedString,
    pub(crate) label: SharedString,
    /// The project a row belongs to, for the two parts that name one.
    pub(crate) detail: SharedString,
    /// The trailing fact: the agent's ladder word for a new session, what a
    /// running one is doing, when a stored one was last touched.
    pub(crate) meta: SharedString,
    /// Only "full control" is accented - the rarest rung is the one worth
    /// pointing at, and four accents in a list are none.
    pub(crate) meta_accent: bool,
    /// The chord that reaches this row from anywhere, when it has one.
    pub(crate) keys: SharedString,
    pub(crate) action: LauncherAction,
    /// What the filter matches against, lowercased once at build time rather
    /// than per keystroke per row.
    pub(crate) haystack: String,
}

impl LauncherRow {
    pub(crate) fn matches(&self, needle: &str) -> bool {
        needle.is_empty() || self.haystack.contains(needle)
    }
}

impl SplitlaneApp {
    /// Push the launcher's rows into every pane showing it, once per frame.
    ///
    /// Built once and cloned into each pane rather than per pane: two launcher
    /// panes are possible (`+ agent` twice with room for both) and they would
    /// hold the same list.
    pub(crate) fn sync_launcher_rows(&mut self, cx: &mut Context<Self>) {
        let Some(ws_idx) = self
            .workspaces
            .get(self.active_idx)
            .map(|_| self.active_idx)
        else {
            return;
        };
        let panes: Vec<Entity<Pane>> = self
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.root.as_ref())
            .map(|root| root.collect_leaves())
            .unwrap_or_default()
            .into_iter()
            .filter(|pane| pane.read(cx).showing_launcher())
            .collect();
        if panes.is_empty() {
            // Nothing is asking, so the next launcher gets a fresh read rather
            // than whatever was on disk the last time one was open.
            self.launcher_history = None;
            return;
        }
        if self.launcher_history.is_none() {
            self.load_launcher_history(ws_idx, cx);
        }
        let rows = self.launcher_rows(ws_idx, cx);
        for pane in panes {
            pane.update(cx, |pane, cx| pane.set_launcher_rows(rows.clone(), cx));
        }
    }

    /// The three parts, in order.
    fn launcher_rows(&self, ws_idx: usize, cx: &gpui::App) -> Vec<LauncherRow> {
        let mut rows: Vec<LauncherRow> = Vec::new();
        // The container's own preferred agent sorts first. Not `⌘N`'s agent -
        // `⌘N` opens this launcher rather than starting anything, which is
        // what the launcher's rewrite made of it, so no row here can claim that chord
        // without promising something the keymap does not do.
        let preferred = self.default_agent_for(ws_idx);
        let new_shell_keys = self
            .shortcut_for_action("new_shell")
            .map(SharedString::from)
            .unwrap_or_default();

        // 1. Start something new, in the design's order: the preferred agent,
        //    then most recently launched, then never used, alphabetical inside
        //    a tie. Length here is set by the machine and not by us - with
        //    nine agents installed this group pushes OPEN SESSIONS below the
        //    fold of a half-width pane, which is the part the launcher was
        //    rewritten for - so what saves it is that the ones you actually
        //    use are at the top and the rest are one filter keystroke away.
        //
        //    Recency comes from the container's own surface records
        //    (`Thread::created_at`), which is already persisted and is read
        //    synchronously, so the list does not re-sort itself a moment after
        //    it opens. Its bound, stated rather than hidden: a session whose
        //    record has been deleted stops counting, so an agent used once and
        //    cleaned up reads as never used.
        let last_launched = |agent: TerminalAgent| -> Option<u64> {
            self.workspaces
                .get(ws_idx)?
                .threads
                .iter()
                .filter(|thread| thread.terminal_agent == Some(agent))
                .map(|thread| thread.created_at)
                .max()
        };
        let mut agents: Vec<TerminalAgent> = TerminalAgent::ALL
            .iter()
            .copied()
            .filter(|agent| agent.is_installed())
            .collect();
        agents.sort_by_key(|agent| {
            let used = last_launched(*agent);
            (
                u8::from(Some(*agent) != preferred),
                u8::from(used.is_none()),
                std::cmp::Reverse(used.unwrap_or(0)),
                agent.display_name(),
            )
        });
        for agent in agents {
            let label = format!("New {} session", agent.display_name());
            let tier = agent.capability_tier();
            rows.push(LauncherRow {
                group: LauncherGroup::New,
                glyph: SharedString::from("\u{25c6}"),
                haystack: label.to_lowercase(),
                label: SharedString::from(label),
                detail: SharedString::default(),
                meta: SharedString::from(tier.word()),
                meta_accent: tier.is_advertised(),
                keys: SharedString::default(),
                action: LauncherAction::NewAgent(agent),
            });
        }
        rows.push(LauncherRow {
            group: LauncherGroup::New,
            glyph: SharedString::from("\u{25b7}"),
            label: SharedString::from("New shell"),
            detail: SharedString::default(),
            meta: SharedString::default(),
            meta_accent: false,
            keys: new_shell_keys,
            action: LauncherAction::NewShell,
            haystack: "new shell terminal".into(),
        });

        // 2. Open sessions - this container's surfaces that no pane is
        //    showing. Ones already in a pane are omitted rather than drawn
        //    disabled: the ladder would send them to their own pane anyway,
        //    so the row would move the eye and change nothing.
        if let Some(container) = self.workspaces.get(ws_idx) {
            let project = SharedString::from(container.title.clone());
            for thread in &container.threads {
                if crate::app::agent_slots::agent_leaf_index(container, thread.id, cx).is_some() {
                    continue;
                }
                let is_shell = crate::app::agents_sidebar::is_shell_surface(thread);
                // The rail's own words for the same three states, so a
                // surface does not change vocabulary by being listed here.
                let meta = if is_shell {
                    SharedString::from("shell")
                } else {
                    crate::app::slot_header::slot_header_status_word(
                        thread.status,
                        crate::theme::ui_colors(),
                    )
                    .0
                };
                rows.push(LauncherRow {
                    group: LauncherGroup::Open,
                    glyph: SharedString::from(if is_shell { "\u{25b7}" } else { "\u{25c6}" }),
                    haystack: thread.title.to_lowercase(),
                    label: SharedString::from(thread.title.clone()),
                    detail: project.clone(),
                    meta_accent: thread.status == crate::project::ThreadStatus::WaitingForInput,
                    meta,
                    keys: SharedString::default(),
                    action: LauncherAction::ShowSurface {
                        thread_id: thread.id,
                    },
                });
            }
        }

        // 3. Recent on disk. `None` while the read is still out - the group
        //    simply is not there yet, which is what a group with no rows looks
        //    like anyway, so a late arrival needs no spinner of its own.
        let open_sessions: std::collections::HashSet<&str> = self
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                ws.threads
                    .iter()
                    .filter_map(|t| t.session_id.as_deref())
                    .collect()
            })
            .unwrap_or_default();
        for row in self.launcher_history.iter().flatten() {
            // A session already open in this container is offered once, above,
            // where it can say what it is doing.
            if open_sessions.contains(row.session_id.as_str()) {
                continue;
            }
            let agent = row.agent.terminal_agent();
            let mut haystack = row.title.to_lowercase();
            if let Some(prompt) = &row.first_prompt {
                haystack.push(' ');
                haystack.push_str(&prompt.to_lowercase());
            }
            rows.push(LauncherRow {
                group: LauncherGroup::Recent,
                glyph: SharedString::from("\u{25c7}"),
                label: SharedString::from(row.title.clone()),
                detail: SharedString::from(row.project.clone()),
                meta: SharedString::from(last_touched_ago(row.last_activity_secs)),
                meta_accent: false,
                keys: SharedString::default(),
                action: LauncherAction::ResumeSession {
                    agent,
                    session_id: row.session_id.clone(),
                    title: row.title.clone(),
                },
                haystack,
            });
        }
        rows
    }

    /// Read this container's session store off the render thread.
    ///
    /// `CLAUDE.md`: `read_sessions_for_cwd` walks and parses a whole project
    /// directory and must never run on the GPUI main thread. Generation-guarded
    /// the same way the palette's History tab is, so a second launcher opening
    /// while a read is in flight discards the older answer instead of racing it
    /// onto the screen.
    fn load_launcher_history(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        self.launcher_history_generation += 1;
        let generation = self.launcher_history_generation;
        let agents = crate::agent_sessions::enabled_session_agents_from_config(&self.cached_config);
        let Some((project, cwd)) = self
            .workspaces
            .get(ws_idx)
            .map(|ws| (ws.title.clone(), ws.cwd.clone()))
        else {
            return;
        };
        // Set now, so the next frame does not start a second read while this
        // one is out.
        self.launcher_history = Some(Vec::new());
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let rows = smol::unblock(move || {
                    let mut rows: Vec<crate::app::command_palette::HistoryRow> = Vec::new();
                    for agent in &agents {
                        for meta in crate::agent_sessions::read_sessions_for_cwd(*agent, &cwd) {
                            rows.push(crate::app::command_palette::HistoryRow {
                                agent: *agent,
                                title: meta
                                    .summary
                                    .clone()
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or_else(|| meta.session_id.chars().take(8).collect()),
                                first_prompt: meta.first_prompt.filter(|s| !s.trim().is_empty()),
                                session_id: meta.session_id,
                                ws_idx,
                                project: project.clone(),
                                branch: meta.git_branch,
                                last_activity_secs: meta.last_activity_secs,
                            });
                        }
                    }
                    // Last activity, never creation time - the same rule the
                    // palette states in bold, for the same reason.
                    rows.sort_by_key(|row| std::cmp::Reverse(row.last_activity_secs));
                    rows.truncate(LAUNCHER_HISTORY_CAP);
                    rows
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        if app.launcher_history_generation != generation {
                            return;
                        }
                        app.launcher_history = Some(rows);
                        cx.notify();
                    })
                });
            },
        )
        .detach();
    }

    /// Fill `pane` with what the picked row names.
    ///
    /// `FOCUS.md`: "Picking a row fills that same pane; focus does not move,
    /// because it was already there." So this does not go through the targeting
    /// ladder - the pane is already chosen, and asking again could put the
    /// session somewhere the user was not looking.
    pub(crate) fn launcher_pick(
        &mut self,
        pane: &Entity<Pane>,
        action: LauncherAction,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_idx) = self.workspace_index_of_pane(pane, cx) else {
            return;
        };
        let thread_id = match action {
            LauncherAction::NewAgent(agent) => {
                // Absent rather than disabled is the list's rule, so a row for
                // an agent that cannot start should not exist. Checked anyway:
                // the list is a frame old and PATH is not ours.
                if !agent.is_installed() {
                    self.show_toast(format!("{} is not installed", agent.display_name()), cx);
                    return;
                }
                match self.add_terminal_thread(ws_idx, agent.display_name(), Some(agent), cx) {
                    Ok(id) => id,
                    Err(err) => {
                        self.show_toast(format!("Could not create session: {err:?}"), cx);
                        return;
                    }
                }
            }
            LauncherAction::NewShell => {
                let title = self
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| Pane::cwd_label(&ws.cwd))
                    .unwrap_or_else(|| "Terminal".to_string());
                match self.add_terminal_thread(ws_idx, title, None, cx) {
                    Ok(id) => id,
                    Err(err) => {
                        self.show_toast(format!("Could not open a shell: {err:?}"), cx);
                        return;
                    }
                }
            }
            LauncherAction::ShowSurface { thread_id } => {
                // Re-checked here and not trusted from the row. "In no pane"
                // was true when the list was built, which is a frame ago, and
                // `adopt_orphan_pane_surfaces` runs immediately before the
                // push - a rail drag or a palette pick in between would put
                // this surface in a pane, and mounting it again would give one
                // PTY two panes. Found by a cross-vendor review.
                //
                // (`agent_leaf_index` covers shells as well as agents: it
                // matches on `agent_thread_id`, which every terminal in a pane
                // carries now, whatever its record says it is.)
                if let Some(container) = self.workspaces.get(ws_idx)
                    && let Some(leaf) =
                        crate::app::agent_slots::agent_leaf_index(container, thread_id, cx)
                    && let Some(existing) = container
                        .root
                        .as_ref()
                        .and_then(|root| root.collect_leaves().get(leaf).cloned())
                {
                    // Already on screen. Go there rather than draw it twice -
                    // which is what the user meant by picking it anyway.
                    if existing != *pane {
                        self.pending_pane_focus = Some(existing);
                        cx.notify();
                    }
                    return;
                }
                thread_id
            }
            LauncherAction::ResumeSession {
                agent,
                session_id,
                title,
            } => {
                if !agent.is_installed() {
                    self.show_toast(format!("{} is not installed", agent.display_name()), cx);
                    return;
                }
                // And nowhere else either. Two containers can point at one
                // directory - the folder picker takes the same folder twice -
                // and the two would share this session's transcript. One
                // transcript, one surface; say so rather than mint a second.
                if let Some((_, other)) = self.workspaces.iter().enumerate().find(|(idx, ws)| {
                    *idx != ws_idx
                        && ws
                            .threads
                            .iter()
                            .any(|t| t.session_id.as_deref() == Some(session_id.as_str()))
                }) {
                    let where_ = other.title.clone();
                    self.show_toast(format!("That session is already open in {where_}"), cx);
                    return;
                }
                // Already open in this container: show that one rather than
                // minting a second surface onto one transcript.
                match self.thread_idx_for_session(ws_idx, &session_id) {
                    Some(idx) => match self
                        .workspaces
                        .get(ws_idx)
                        .and_then(|ws| ws.threads.get(idx))
                    {
                        Some(thread) => thread.id,
                        None => return,
                    },
                    None => match self.add_terminal_thread_for_session(
                        ws_idx,
                        title,
                        Some(agent),
                        &session_id,
                        cx,
                    ) {
                        Ok(id) => id,
                        Err(err) => {
                            self.show_toast(format!("Could not open session: {err:?}"), cx);
                            return;
                        }
                    },
                }
            }
        };
        // Both of these run after a surface may already have been created, so
        // neither may return in silence: the process would be alive, the pane
        // would still show the launcher, and `enter` would look like it did
        // nothing. The surface keeps its rail row either way, which is where
        // the user finds it.
        let Some(thread_idx) = self.thread_index_by_id(ws_idx, thread_id) else {
            self.show_toast("That session is no longer there", cx);
            return;
        };
        let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
        let Some(view) = self.mount_agents_terminal_for_target(target, cx) else {
            self.show_toast("Could not open a terminal for that session", cx);
            return;
        };
        pane.update(cx, |pane, cx| {
            pane.show_terminal(view, cx);
            pane.set_launching(false, cx);
            pane.clear_launcher_query(cx);
        });
        self.active_idx = ws_idx;
        self.pending_pane_focus = Some(pane.clone());
        self.reroot_files_tree(cx);
        self.save_session(cx);
        cx.notify();
    }
}

/// How many stored sessions the third part holds. A launcher is a list you
/// scan; the filter is the way past the cut.
const LAUNCHER_HISTORY_CAP: usize = 60;

/// When a stored session was last touched, in the coarsest unit that is still
/// true.
///
/// The design writes "closed yesterday 17:12". Two departures. A wall-clock
/// time is a second thing to read and the row is already three columns wide.
/// And **"closed" would be a claim this cannot make**: these rows come from
/// the CLI's own on-disk store, which records when a session was last written
/// and nothing about whether it is running - it may be live in another
/// Splitlane window, or in a terminal outside one. The age is the fact; the
/// group heading already says where it came from.
fn last_touched_ago(last_activity_secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let elapsed = (now - last_activity_secs).max(0);
    if elapsed < 60 {
        return "just now".to_string();
    }
    let (n, unit) = if elapsed < 3600 {
        (elapsed / 60, "minute")
    } else if elapsed < 86_400 {
        (elapsed / 3600, "hour")
    } else if elapsed < 2_592_000 {
        (elapsed / 86_400, "day")
    } else {
        (elapsed / 2_592_000, "month")
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::last_touched_ago;

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
    }

    #[test]
    fn last_touched_ago_names_the_coarsest_true_unit() {
        assert_eq!(last_touched_ago(now()), "just now");
        assert_eq!(last_touched_ago(now() - 90), "1 minute ago");
        assert_eq!(last_touched_ago(now() - 7_200), "2 hours ago");
        assert_eq!(last_touched_ago(now() - 90_000), "1 day ago");
        assert_eq!(last_touched_ago(now() - 5_400_000), "2 months ago");
    }

    #[test]
    fn last_touched_ago_never_reads_from_the_future() {
        // A file written by a machine whose clock is ahead would otherwise
        // produce a negative count, which reads as a bug in the app rather
        // than as a fact about the file.
        assert_eq!(last_touched_ago(now() + 10_000), "just now");
    }
}
