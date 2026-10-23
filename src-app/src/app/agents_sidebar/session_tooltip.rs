//! A session row's hover: what the session is, and what the person wrote in it.
//!
//! A title is written once, near a session's start, and by its tenth prompt it
//! may say nothing about where the session got to. The person's own prompts
//! do, and they can be shown without the app claiming anything: no summary is
//! made, the lines are what was typed, with when. The first prompt says what
//! the session set out to do and the last few where it is now; the ones
//! between are counted rather than listed, so the card stays a glance.
//!
//! The rows are read from the transcript off the render thread, the first
//! time the card is shown, and from where the last read stopped after that.
//! What an earlier read found is drawn at once, and the card fills in when the
//! read comes back.

use std::path::PathBuf;

use gpui::{
    AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, Styled, Task, Window,
    div,
};

use crate::agent_sessions::SessionAgent;
use crate::prompt_index::OutlineLine;
use crate::ui_primitives::LABEL;
use crate::ui_tokens as tok;

/// Where a session's prompts are written.
#[derive(Clone)]
pub(crate) enum PromptSource {
    /// A Claude Code transcript, whose path is composed from the session.
    Claude(PathBuf),
    /// A Codex session, whose rollout is found by its id: the file name
    /// carries the date it started, which the app does not keep.
    Codex(String),
}

/// What a session row says on hover.
#[derive(Clone)]
pub(crate) struct RowTooltip {
    /// The title, then the branch and port when the session has its own.
    pub(crate) head: Vec<String>,
<<<<<<< HEAD
    /// The session's own worktree, whose work is counted under the branch.
    pub(crate) worktree: Option<crate::workspace::worktree::ManagedWorktree>,
=======
>>>>>>> origin/main
    pub(crate) prompts: Option<PromptSource>,
}

pub(crate) fn row_tooltip(tip: RowTooltip) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_w, cx| cx.new(|cx| SessionTooltip::new(tip.clone(), cx)).into()
}

struct SessionTooltip {
    head: Vec<gpui::SharedString>,
    outline: Vec<OutlineLine>,
<<<<<<< HEAD
    /// What the branch holds - `3 changed files · 2 commits` - once counted,
    /// and only when it holds something.
    work: Option<gpui::SharedString>,
    /// Dropped with the card, which cancels a read nobody is waiting for.
    _read: Option<Task<()>>,
    _count: Option<Task<()>>,
=======
    /// Dropped with the card, which cancels a read nobody is waiting for.
    _read: Option<Task<()>>,
>>>>>>> origin/main
}

impl SessionTooltip {
    fn new(tip: RowTooltip, cx: &mut Context<Self>) -> Self {
        let outline = match &tip.prompts {
            Some(PromptSource::Claude(path)) => crate::prompt_index::cached(path)
                .map(|prompts| prompts.outline())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let read = tip.prompts.map(|source| {
            cx.spawn(async move |this, cx| {
                let outline = smol::unblock(move || {
                    let (agent, path) = match source {
                        PromptSource::Claude(path) => (SessionAgent::Claude, path),
                        PromptSource::Codex(id) => (
                            SessionAgent::Codex,
                            crate::codex_state::rollout_path_for(&id)?,
                        ),
                    };
                    Some(crate::prompt_index::prompts_of(agent, &path).outline())
                })
                .await;
                if let Some(outline) = outline {
                    let _ = this.update(cx, |tooltip, cx| {
                        tooltip.outline = outline;
                        cx.notify();
                    });
                }
            })
        });
<<<<<<< HEAD
        let count = tip.worktree.map(|wt| {
            cx.spawn(async move |this, cx| {
                let work =
                    smol::unblock(move || crate::workspace::session_branch::branch_work(&wt)).await;
                let Some(summary) = work
                    .ok()
                    .and_then(|work| crate::workspace::session_branch::work_summary(&work))
                else {
                    return;
                };
                let _ = this.update(cx, |tooltip, cx| {
                    tooltip.work = Some(summary.into());
                    cx.notify();
                });
            })
        });
        Self {
            head: tip.head.into_iter().map(Into::into).collect(),
            outline,
            work: None,
            _read: read,
            _count: count,
=======
        Self {
            head: tip.head.into_iter().map(Into::into).collect(),
            outline,
            _read: read,
>>>>>>> origin/main
        }
    }
}

impl Render for SessionTooltip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::active_theme();
        let ui = crate::theme::ui_colors();
        let now = chrono::Local::now();
<<<<<<< HEAD
        // The title, the branch, what it holds, then the port.
        let mut lines = self.head.clone();
        if let Some(work) = &self.work {
            lines.insert(lines.len().min(2), work.clone());
        }
        let head = lines.into_iter().enumerate().map(|(i, line)| {
=======
        let head = self.head.iter().enumerate().map(|(i, line)| {
>>>>>>> origin/main
            div()
                .text_color(if i == 0 { ui.text } else { ui.text_tertiary })
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
<<<<<<< HEAD
                .child(line)
=======
                .child(line.clone())
>>>>>>> origin/main
        });
        let prompts = (!self.outline.is_empty()).then(|| {
            div()
                .mt(tok::space::XS)
                .pt(tok::space::SM)
                .border_t_1()
                .border_color(ui.divider)
                .flex()
                .flex_col()
                .gap(tok::space::XS)
                .children(self.outline.iter().map(|line| {
                    match line {
                        OutlineLine::Prompt { at, text } => div()
                            .flex()
                            .gap(tok::space::MD)
                            .child(
                                div()
                                    .flex_none()
                                    .w(tok::popover::PROMPT_TIME)
                                    .text_color(ui.text_tertiary)
                                    .child(at.map(|at| when(at, now)).unwrap_or_default()),
                            )
                            .child(
                                // One line each: the card is a list to glance
                                // down, and a wrapped prompt reads as two.
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_color(ui.text_secondary)
                                    .child(text.clone()),
                            ),
                        OutlineLine::Skipped(count) => div()
                            .pl(tok::popover::PROMPT_TIME + tok::space::MD)
                            .text_color(ui.text_tertiary)
                            .child(format!(
                                "{count} more {}",
                                if *count == 1 { "message" } else { "messages" }
                            )),
                    }
                }))
        });
        div()
            .max_w(tok::popover::SESSION_TOOLTIP_MAX)
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::SMALL)
            .bg(theme.title_bar_background)
            .border_1()
            .border_color(ui.border)
            .text_size(LABEL)
            .flex()
            .flex_col()
            .gap(tok::space::XS)
            .children(head)
            .children(prompts)
    }
}

/// When a prompt was sent, as short as still tells it apart: the time today,
/// the day this year, the date before that.
fn when(at: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, TimeZone};
    let Some(at) = chrono::Local.timestamp_opt(at, 0).single() else {
        return String::new();
    };
    if at.date_naive() == now.date_naive() {
        at.format("%H:%M").to_string()
    } else if at.year() == now.year() {
        at.format("%b %-d").to_string()
    } else {
        at.format("%b %Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_prompt_is_dated_as_briefly_as_tells_it_apart() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 10, 23, 18, 0, 0)
            .single()
            .unwrap();
        let at = |y, m, d, h, min| {
            chrono::Local
                .with_ymd_and_hms(y, m, d, h, min, 0)
                .single()
                .unwrap()
                .timestamp()
        };
        assert_eq!(when(at(2026, 10, 23, 9, 5), now), "09:05");
        assert_eq!(when(at(2026, 10, 21, 9, 5), now), "Oct 21");
        assert_eq!(when(at(2025, 12, 30, 9, 5), now), "Dec 2025");
    }
}
