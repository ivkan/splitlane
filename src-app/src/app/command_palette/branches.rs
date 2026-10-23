//! The palette's Branches tab: what sessions left behind.
//!
//! A session closed with work in its branch either kept the branch or was
//! deleted with a snapshot taken first. Both are found again here, by the
//! session's name rather than by `splitlane/ab12cd`: the `splitlane/*`
//! branches no open session works on, and the snapshots not yet past their
//! thirty days. Branches a person made are not listed; git is where those
//! live. The tab is not shown while it has nothing in it.
//!
//! Enter opens a new session on the branch, in its worktree, made again if
//! it was removed; on a snapshot it brings the snapshot back into a new
//! branch first. `⌘⌫` on a branch deletes it the way the delete card's
//! `Delete everything` does, snapshot and all. On a snapshot it asks first,
//! on a red card: nothing of the work is left after that one.

use gpui::Context;

use crate::SplitlaneApp;
use crate::workspace::session_branch::{self, LeftBehind, LeftKind};

use super::{PaletteItem, PaletteTab};

/// A snapshot the person asked to delete, waiting on the card.
pub(crate) struct DropSnapshot {
    repo_root: std::path::PathBuf,
    reference: String,
    branch: String,
    title: String,
    holds: String,
}

/// One row of the Branches tab.
pub(crate) struct BranchRow {
    pub(crate) item: LeftBehind,
    /// The project it belongs to, by index and by name.
    pub(crate) ws_idx: usize,
    pub(crate) project: String,
    /// What is in it: `2 commits · 4 changed files`.
    pub(crate) holds: String,
}

impl SplitlaneApp {
    /// Read what sessions left behind in every open git project, off the
    /// render thread. Called when the palette opens, so the tab can appear
    /// only when it has something to show.
    pub(crate) fn load_palette_branches(&mut self, cx: &mut Context<Self>) {
        let mut repos: Vec<(usize, String, std::path::PathBuf, Vec<String>)> = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            let Some(repo_root) = ws.repo_root.clone() else {
                continue;
            };
            if repos.iter().any(|(_, _, root, _)| *root == repo_root) {
                continue;
            }
            // A branch an open session works in is not left behind.
            let live: Vec<String> = self
                .workspaces
                .iter()
                .flat_map(|ws| {
                    ws.managed_worktrees.iter().filter(|wt| {
                        ws.threads
                            .iter()
                            .any(|t| std::path::Path::new(&t.cwd).starts_with(&wt.path))
                    })
                })
                .map(|wt| wt.branch.clone())
                .collect();
            repos.push((ws_idx, ws.title.clone(), repo_root, live));
        }
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let rows = smol::unblock(move || {
                    let mut rows = Vec::new();
                    for (ws_idx, project, repo_root, live) in repos {
                        for item in session_branch::left_behind(&repo_root, &live) {
                            let holds = session_branch::what_is_left(&item);
                            rows.push(BranchRow {
                                item,
                                ws_idx,
                                project: project.clone(),
                                holds,
                            });
                        }
                    }
                    rows.sort_by_key(|row| std::cmp::Reverse(row.item.when_secs));
                    rows
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        if let Some(state) = &mut app.command_palette {
                            state.branches = Some(rows);
                            cx.notify();
                        }
                    })
                });
            },
        )
        .detach();
    }

    /// The Branches tab's rows for `needle`, matched on the session's name,
    /// the branch and the project.
    pub(crate) fn palette_branch_items(&self, needle: &str) -> Vec<PaletteItem> {
        let Some(rows) = self
            .command_palette
            .as_ref()
            .and_then(|state| state.branches.as_ref())
        else {
            return Vec::new();
        };
        rows.iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty()
                    || row.item.title.to_lowercase().contains(needle)
                    || row.project.to_lowercase().contains(needle)
                    || branch_name(&row.item).to_lowercase().contains(needle)
            })
            .map(|(index, _)| PaletteItem::Branch { index })
            .collect()
    }

    /// Open a new session on what the row names.
    pub(crate) fn open_palette_branch_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self
            .command_palette
            .as_ref()
            .and_then(|state| state.branches.as_ref())
            .and_then(|rows| rows.get(index))
        else {
            return;
        };
        let (ws_idx, item) = (row.ws_idx, row.item.clone());
        let Some(container_id) = self.workspaces.get(ws_idx).map(|ws| ws.id) else {
            return;
        };
        let held: Vec<u16> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.managed_worktrees.iter().filter_map(|wt| wt.port))
            .collect();
        self.command_palette = None;
        cx.notify();
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let title = item.title.clone();
                let snapshot = match &item.kind {
                    LeftKind::Snapshot { reference, .. } => {
                        Some((item.repo_root.clone(), reference.clone()))
                    }
                    LeftKind::Branch { .. } => None,
                };
                let made = smol::unblock(move || {
                    let mut made = match &item.kind {
                        LeftKind::Branch { branch } => {
                            session_branch::reopen_branch(&item.repo_root, branch)?
                        }
                        LeftKind::Snapshot { reference, .. } => {
                            session_branch::restore_snapshot(&item.repo_root, reference)?
                        }
                    };
                    made.port = crate::workspace::worktree::allocate_port(&held);
                    Ok::<_, String>(made)
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        app.palette_branch_opened(container_id, title, made, snapshot, cx);
                    })
                });
            },
        )
        .detach();
    }

    fn palette_branch_opened(
        &mut self,
        container_id: u64,
        title: String,
        made: Result<crate::workspace::worktree::ManagedWorktree, String>,
        snapshot: Option<(std::path::PathBuf, String)>,
        cx: &mut Context<Self>,
    ) {
        let made = match made {
            Ok(made) => made,
            Err(error) => {
                self.show_toast(format!("Could not open it: {error}"), cx);
                return;
            }
        };
        let Some(ws_idx) = self.workspaces.iter().position(|ws| ws.id == container_id) else {
            // The snapshot stays: the worktree it went into has no owner.
            self.show_toast(
                format!("Its project closed; the work is at {}", made.path.display()),
                cx,
            );
            return;
        };
        let path = made.path.clone();
        if !self.workspaces[ws_idx]
            .managed_worktrees
            .iter()
            .any(|wt| wt.path == made.path)
        {
            self.workspaces[ws_idx].managed_worktrees.push(made);
        }
        self.save_session(cx);
        // Recorded and saved: the snapshot is now the worktree.
        if let Some((repo_root, reference)) = snapshot {
            cx.background_executor()
                .spawn(async move { session_branch::drop_snapshot(&repo_root, &reference) })
                .detach();
        }
        let agent = self
            .default_agent_for(ws_idx)
            .unwrap_or(crate::agent_launcher::TerminalAgent::ClaudeCode);
        self.create_agent_session_in_cwd(ws_idx, agent, title, path, cx);
    }

    /// `⌘⌫` on a row: a branch goes the way `Delete everything` takes it,
    /// with a snapshot of what it held.
    pub(crate) fn delete_palette_branch_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self
            .command_palette
            .as_ref()
            .and_then(|state| state.branches.as_ref())
            .and_then(|rows| rows.get(index))
        else {
            return;
        };
        let branch = match &row.item.kind {
            LeftKind::Branch { branch } => branch,
            LeftKind::Snapshot { reference, branch } => {
                self.drop_snapshot_asked = Some(DropSnapshot {
                    repo_root: row.item.repo_root.clone(),
                    reference: reference.clone(),
                    branch: branch.clone(),
                    title: row.item.title.clone(),
                    holds: row.holds.clone(),
                });
                // The card is answered with the pointer; the palette would
                // keep the keyboard under it.
                self.command_palette = None;
                cx.notify();
                return;
            }
        };
        // The list was read when the palette opened; a session may have
        // opened on the branch since.
        let live = self.workspaces.iter().any(|ws| {
            ws.managed_worktrees.iter().any(|wt| {
                wt.branch == *branch
                    && ws
                        .threads
                        .iter()
                        .any(|t| std::path::Path::new(&t.cwd).starts_with(&wt.path))
            })
        });
        if live {
            self.show_toast(format!("{branch} has a session open in it"), cx);
            return;
        }
        let (repo_root, branch, title) = (
            row.item.repo_root.clone(),
            branch.clone(),
            row.item.title.clone(),
        );
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let shown = branch.clone();
                let result = smol::unblock(move || {
                    let wt = session_branch::left_branch_worktree(&repo_root, &branch)?;
                    session_branch::discard(&wt, &title)
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        match result {
                            Ok(_) => app.show_toast(
                                format!(
                                    "Deleted {shown} - a snapshot is kept for {} days",
                                    session_branch::SNAPSHOT_DAYS
                                ),
                                cx,
                            ),
                            Err(error) => app.show_toast(format!("{shown} kept: {error}"), cx),
                        }
                        app.load_palette_branches(cx);
                    })
                });
            },
        )
        .detach();
    }

    pub(crate) fn render_drop_snapshot_confirm(
        &self,
        asked: &DropSnapshot,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use crate::app::confirm_dialog::{ConfirmDialog, ConfirmTone, render_confirm_dialog};
        let (title, rest) = drop_snapshot_copy(asked);
        render_confirm_dialog(
            ConfirmDialog {
                id: "drop-snapshot-confirm",
                title,
                body: "The snapshot of".to_string(),
                body_code: Some((asked.branch.clone(), rest)),
                note: None,
                confirm_label: "Delete snapshot".to_string(),
                safe: None,
                focus: None,
                on_cancel: std::rc::Rc::new(|this, window, cx| {
                    this.drop_snapshot_asked = None;
                    this.handle_open_branches(&crate::OpenBranches, window, cx);
                }),
                on_confirm: std::rc::Rc::new(|this, _window, cx| this.drop_asked_snapshot(cx)),
                aside: None,
                tone: ConfirmTone::Danger,
            },
            ui,
            cx,
        )
    }

    fn drop_asked_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(asked) = self.drop_snapshot_asked.take() else {
            return;
        };
        cx.notify();
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let DropSnapshot {
                    repo_root,
                    reference,
                    title,
                    ..
                } = asked;
                smol::unblock(move || session_branch::drop_snapshot(&repo_root, &reference)).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        app.show_toast(format!("Snapshot of \"{title}\" deleted"), cx);
                    })
                });
            },
        )
        .detach();
    }

    /// The `Show` on a kept-branch toast: the palette, on this tab.
    pub(crate) fn handle_open_branches(
        &mut self,
        _: &crate::OpenBranches,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_none() {
            self.open_command_palette(window, cx);
        }
        if let Some(state) = &mut self.command_palette {
            state.tab = PaletteTab::Branches;
            state.selected = 0;
        }
        cx.notify();
    }
}

/// The card's title, and what follows the branch's name in its body. Red,
/// unlike `Delete everything`: that one leaves a snapshot behind, and this
/// is the snapshot.
fn drop_snapshot_copy(asked: &DropSnapshot) -> (String, String) {
    (
        format!("Delete the snapshot of \"{}\"?", asked.title),
        format!(" holds {}. Nothing of it is left after this.", asked.holds),
    )
}

/// The branch a row is about, or the one a snapshot was taken of.
pub(crate) fn branch_name(item: &LeftBehind) -> &str {
    match &item.kind {
        LeftKind::Branch { branch } | LeftKind::Snapshot { branch, .. } => branch,
    }
}
