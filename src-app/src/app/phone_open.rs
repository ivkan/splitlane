//! Agent sessions started from a paired phone.
//!
//! The person holding the phone is the person at the computer: pairing, with
//! a code typed on this computer, is their consent. So a session started from
//! the phone is started the way the launcher starts one - its own branch and
//! worktree where the project gives sessions one, no opener above it - and
//! not the way a program outside a pane opens one. What differs is what the
//! person did not see: the row says which phone started it until the session
//! has been on screen at the computer, and every start is announced at once.
//!
//! A phone left unlocked, or taken, can spend the person's tokens, so starts
//! are bounded: a few running at once, and not too often from one phone. The
//! reasoning is in `docs/internals/design-decisions.md`, "A phone starts a
//! session as the person does".

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity};

use crate::SplitlaneApp;
use crate::app::ipc_handler::JsonRpcError;
use crate::app::notifications::ToastAction;
use crate::terminal::view::TerminalView;

/// How many sessions started from phones may run at once.
pub(crate) const MAX_PHONE_SESSIONS: usize = 4;
/// How soon after one start the same phone may start another.
pub(crate) const PHONE_START_GAP: Duration = Duration::from_secs(15);
/// How many sessions one phone may start in an hour.
pub(crate) const PHONE_STARTS_PER_HOUR: usize = 12;
/// The longest first message a phone may give. Nobody types more on a phone,
/// and the first message is what a session spends its tokens on.
pub(crate) const MAX_PHONE_PROMPT_BYTES: usize = 8 * 1024;

const HOUR: Duration = Duration::from_secs(60 * 60);

/// The request a phone starts a session with, as the journal names it.
const METHOD: &str = "surface.open_from_phone";

/// What the window keeps about starts from phones while it runs.
#[derive(Default)]
pub(crate) struct PhoneStarts {
    /// When each phone started its sessions in the last hour, by its name.
    by_phone: HashMap<String, Vec<Instant>>,
    /// Sessions whose worktree is still being made, with the first message
    /// each is to be given once its terminal starts.
    awaiting_worktree: HashMap<u64, Option<String>>,
}

impl PhoneStarts {
    /// Whether `phone` may start a session now, with `running` sessions
    /// started from phones running; the reason in words when it may not.
    pub(crate) fn may_start(
        &self,
        phone: &str,
        running: usize,
        now: Instant,
    ) -> Result<(), String> {
        if running >= MAX_PHONE_SESSIONS {
            return Err(format!(
                "{MAX_PHONE_SESSIONS} sessions started from a phone are running; \
                 close one of them first"
            ));
        }
        let recent: Vec<&Instant> = self
            .by_phone
            .get(phone)
            .into_iter()
            .flatten()
            .filter(|at| now.saturating_duration_since(**at) < HOUR)
            .collect();
        if let Some(last) = recent.iter().max() {
            let since = now.saturating_duration_since(**last);
            if since < PHONE_START_GAP {
                let wait = (PHONE_START_GAP - since).as_secs().max(1);
                return Err(format!("wait {wait} s before starting another session"));
            }
        }
        if recent.len() >= PHONE_STARTS_PER_HOUR {
            return Err(format!(
                "this phone started {PHONE_STARTS_PER_HOUR} sessions in the last hour; \
                 start the next one at the computer"
            ));
        }
        Ok(())
    }

    fn note_start(&mut self, phone: &str, now: Instant) {
        let starts = self.by_phone.entry(phone.to_string()).or_default();
        starts.retain(|at| now.saturating_duration_since(*at) < HOUR);
        starts.push(now);
    }

    /// The first message for a session whose worktree was waited for, when
    /// a phone started it; `None` for any other session.
    pub(crate) fn take_awaiting(&mut self, thread_id: u64) -> Option<Option<String>> {
        self.awaiting_worktree.remove(&thread_id)
    }
}

impl SplitlaneApp {
    /// `surface.open_from_phone`: start an agent session for a paired phone,
    /// through the bridge this window started.
    pub(crate) fn handle_open_from_phone(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        let Some(phone) = crate::remote::verified_device(params).map(str::to_string) else {
            return JsonRpcError::invalid_params(
                "only the phone bridge Splitlane started may start a session for a phone",
            )
            .into_value();
        };
        let tag = params.get("agent").and_then(|a| a.as_str()).unwrap_or("");
        let Some(agent) = crate::agent_launcher::TerminalAgent::from_tag(tag)
            .or_else(|| crate::agent_launcher::TerminalAgent::from_binary(tag))
        else {
            return JsonRpcError::invalid_params(format!("no agent called `{tag}`")).into_value();
        };
        let refuse = |reason: &str| {
            crate::orchestration_journal::record(
                "refused",
                crate::orchestration_journal::Session {
                    agent: Some(agent.binary()),
                    ..Default::default()
                },
                serde_json::json!({ "method": METHOD, "reason": reason, "caller": "phone" }),
            );
        };
        if !agent.is_installed() {
            refuse("not_installed");
            return JsonRpcError::invalid_params(format!(
                "{} is not installed on this computer",
                agent.display_name()
            ))
            .into_value();
        }
        let prompt = params
            .get("prompt")
            .and_then(|p| p.as_str())
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        if prompt
            .as_ref()
            .is_some_and(|p| p.len() > MAX_PHONE_PROMPT_BYTES)
        {
            return JsonRpcError::invalid_params(format!(
                "the first message is longer than {} KiB",
                MAX_PHONE_PROMPT_BYTES / 1024
            ))
            .into_value();
        }
        // By the project's own id: an index changes when projects are
        // reordered, and two projects may have one title.
        let Some(ws_idx) = params
            .get("project_id")
            .and_then(|id| id.as_u64())
            .and_then(|id| self.workspaces.iter().position(|ws| ws.id == id))
        else {
            refuse("no_project");
            return JsonRpcError::invalid_params(
                "that project is not open on the computer any more",
            )
            .into_value();
        };
        let now = Instant::now();
        let running = self.running_phone_sessions(cx);
        if let Err(why) = self.phone_starts.may_start(&phone, running, now) {
            refuse("phone_limit");
            return JsonRpcError::invalid_params(why).into_value();
        }

        let thread_id =
            match self.add_terminal_thread(ws_idx, agent.display_name(), Some(agent), cx) {
                Ok(id) => id,
                Err(err) => {
                    return JsonRpcError::invalid_params(format!(
                        "could not start a session: {err:?}"
                    ))
                    .into_value();
                }
            };
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.opened_from_phone = Some(splitlane_config::schema::OpenedFromPhone {
                phone: phone.clone(),
                seen: false,
            });
        }
        // Counted from here on, whatever becomes of it: a start that fails
        // late still cost what it cost.
        self.phone_starts.note_start(&phone, now);
        let project = self.workspaces[ws_idx].title.clone();
        // It stays up long enough to be read by someone coming back to the
        // computer, and offers the way to take the phone's rights away: this
        // is the moment a person learns of a start that was not theirs.
        self.push_toast(
            format!(
                "{phone} (phone) started a {} session in {project}",
                agent.display_name()
            ),
            vec![ToastAction::PhoneSettings],
            crate::app::constants::TOAST_HOLD_MS * 40,
            cx,
        );

        // Its terminal starts once its worktree is made; the completion
        // starts it without putting it on screen.
        if self.give_session_own_worktree(ws_idx, thread_id, cx) {
            self.phone_starts
                .awaiting_worktree
                .insert(thread_id, prompt);
            self.save_session(cx);
            cx.notify();
            return serde_json::json!({ "thread_id": thread_id, "surface_id": null });
        }
        match self.start_phone_session(thread_id, prompt, cx) {
            Some(view) => serde_json::json!({
                "thread_id": thread_id,
                "surface_id": view.entity_id().as_u64(),
            }),
            None => {
                self.journal_refusal_from_phone(thread_id, "not_started");
                if let Some(thread_idx) = self.thread_index_by_id(ws_idx, thread_id) {
                    let _ = self.remove_thread(ws_idx, thread_idx, cx);
                }
                JsonRpcError::invalid_params("the session could not be started").into_value()
            }
        }
    }

    /// Start the terminal of a session a phone asked for, in the rail and
    /// not on screen: the person at the computer, if there is one, is doing
    /// something else. The first message goes to the agent once it is there.
    pub(crate) fn start_phone_session(
        &mut self,
        thread_id: u64,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.threads.iter().any(|t| t.id == thread_id))?;
        let thread_idx = self.thread_index_by_id(ws_idx, thread_id)?;
        let agent = self.workspaces[ws_idx].threads[thread_idx].terminal_agent?;
        let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
        let (view, at_launch) =
            self.mount_with_opening_text(target, thread_id, agent, prompt.as_deref(), true, cx)?;
        // A session in the rail is never drawn, so nothing else starts it.
        view.update(cx, |view, cx| view.ensure_backend_started(cx));
        let mut opened = self.journal_place(thread_id);
        if let Some(fields) = opened.as_object_mut() {
            fields.insert("via".into(), METHOD.into());
            fields.insert("surface_id".into(), view.entity_id().as_u64().into());
            fields.insert("caller".into(), "phone".into());
            fields.insert("placement".into(), "parked".into());
            fields.insert(
                "prompt_bytes".into(),
                prompt.as_ref().map(String::len).into(),
            );
            fields.insert("submit".into(), true.into());
            fields.insert("prompt_at_launch".into(), at_launch.is_some().into());
        }
        self.journal("open", thread_id, opened);
        if let Some(text) = prompt {
            self.deliver_opening_text(thread_id, &view, text, at_launch, true, false, cx);
        }
        self.save_session(cx);
        cx.notify();
        Some(view)
    }

    /// A phone's session whose worktree could not be made starts in the
    /// project folder: nobody may be at the computer to choose, and the
    /// person asked for a session, not for a branch.
    pub(crate) fn start_phone_session_without_branch(
        &mut self,
        thread_id: u64,
        prompt: Option<String>,
        error: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.worktree_pending = None;
            thread.worktree_failed = None;
        }
        self.show_toast(
            format!(
                "No branch for the session from the phone ({error}); it runs in the project folder"
            ),
            cx,
        );
        self.start_phone_session_or_drop(thread_id, prompt, cx);
    }

    /// [`Self::start_phone_session`], and when no terminal can be made, the
    /// row goes: a session that never started is not left in the rail, and
    /// its absence is what tells the phone it failed.
    pub(crate) fn start_phone_session_or_drop(
        &mut self,
        thread_id: u64,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.start_phone_session(thread_id, prompt, cx).is_some() {
            return;
        }
        self.journal_refusal_from_phone(thread_id, "not_started");
        if let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|ws| ws.threads.iter().any(|t| t.id == thread_id))
            && let Some(thread_idx) = self.thread_index_by_id(ws_idx, thread_id)
        {
            let _ = self.remove_thread(ws_idx, thread_idx, cx);
            self.save_session(cx);
        }
        self.show_toast("The session from the phone could not be started", cx);
    }

    fn journal_refusal_from_phone(&self, thread_id: u64, reason: &str) {
        self.journal(
            "refused",
            thread_id,
            serde_json::json!({
                "method": METHOD,
                "reason": reason,
                "caller": "phone",
            }),
        );
    }

    /// How many sessions started from phones are running, those still
    /// waiting for their worktree included.
    fn running_phone_sessions(&self, cx: &App) -> usize {
        let running: std::collections::HashSet<u64> = self
            .held_terminals(cx)
            .into_iter()
            .filter(|(terminal, _)| terminal.read(cx).terminal.exited.is_none())
            .filter_map(|(_, thread_id)| thread_id)
            .collect();
        let started = self
            .workspaces
            .iter()
            .flat_map(|container| container.threads.iter())
            .filter(|thread| {
                thread.opened_from_phone.is_some()
                    && !thread.rail.agent_exited
                    && running.contains(&thread.id)
            })
            .count();
        // Only those whose row is still there: a wait that lost its row
        // must not hold a place for ever.
        let awaiting = self
            .phone_starts
            .awaiting_worktree
            .keys()
            .filter(|id| self.thread_by_id(**id).is_some())
            .count();
        started + awaiting
    }

    /// The person had these sessions on screen at the computer: those a
    /// phone started stop saying so in place of their agent's name.
    pub(crate) fn note_phone_sessions_seen(&mut self, thread_ids: &[u64]) -> bool {
        let mut changed = false;
        for thread_id in thread_ids {
            if let Some(from) = self
                .thread_by_id_mut(*thread_id)
                .and_then(|thread| thread.opened_from_phone.as_mut())
                .filter(|from| !from.seen)
            {
                from.seen = true;
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_starts_a_few_and_not_too_often() {
        let mut starts = PhoneStarts::default();
        let t0 = Instant::now();
        assert!(starts.may_start("Pixel", 0, t0).is_ok());
        starts.note_start("Pixel", t0);
        // Too soon after the last one, from the same phone only.
        let soon = t0 + Duration::from_secs(5);
        let refused = starts.may_start("Pixel", 1, soon).unwrap_err();
        assert!(refused.contains("wait 10 s"), "{refused}");
        assert!(starts.may_start("iPhone", 1, soon).is_ok());
        assert!(starts.may_start("Pixel", 1, t0 + PHONE_START_GAP).is_ok());
        // However long ago the last start, not past the ceiling.
        assert!(
            starts
                .may_start("iPhone", MAX_PHONE_SESSIONS, soon)
                .unwrap_err()
                .contains("close one")
        );
    }

    #[test]
    fn a_phone_starts_at_most_so_many_in_an_hour() {
        let mut starts = PhoneStarts::default();
        let t0 = Instant::now();
        let mut at = t0;
        for _ in 0..PHONE_STARTS_PER_HOUR {
            assert!(starts.may_start("Pixel", 0, at).is_ok());
            starts.note_start("Pixel", at);
            at += PHONE_START_GAP;
        }
        assert!(
            starts
                .may_start("Pixel", 0, at)
                .unwrap_err()
                .contains("last hour")
        );
        // An hour after the first, the first no longer counts.
        assert!(starts.may_start("Pixel", 0, t0 + HOUR).is_ok());
    }
}
