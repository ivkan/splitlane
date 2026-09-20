//! Prompt Composer (CLI Cockpit).
//!
//! A multi-line prompt bar anchored to the bottom edge of the
//! focused pane. Delivery goes through the hardened text injection path
//! (`TerminalView::inject_text`) so embedded newlines stay literal - the
//! prompt lands PRE-FILLED in the agent's input box and is NEVER submitted
//! by default. `secondary-enter` is the explicit, documented
//! deliver-then-submit gesture (a separate `\r` write), unavailable in
//! broadcast mode.
//!
//! Targeting a generating (`Thinking`) session - single-pane or any
//! broadcast member - queues the prompt in a per-pane latest-wins buffer
//! (`BroadcastState::pending`) flushed on the session's next transition out
//! of `Thinking`. The flush runs in [`SplitlaneApp::agent_sessions_changed`],
//! called from the `ai.*` hook handlers on the GPUI main thread (serialized
//! no race between transition and flush), and only ever pre-fills.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Entity, SharedString, WeakEntity, Window};

use crate::SplitlaneApp;
use crate::app::broadcast::state_blocks_delivery;
use crate::app::ipc_handler::find_terminal_by_surface_id;
use crate::pane::Pane;
use crate::widgets::text_area::TextArea;

/// The broadcast recap (and the queued confirmation) hold for
/// 4 s before auto-dismiss - longer than the default `TOAST_HOLD_MS`
/// confirmations so the delivered/queued split is actually readable.
const COMPOSER_RECAP_HOLD_MS: u64 = 4000;

/// Maximum delivered/queued prompt size - parity with the IPC
/// `surface.send_text` 64 KiB cap (`MAX_TEXT_LEN`, ipc_handler.rs).
const MAX_COMPOSER_TEXT: usize = 64 * 1024;

/// Never-submit hardening (security review): the delivery profile is LF-only.
/// A CR/CRLF smuggled into the TextArea through a clipboard paste is
/// normalized to LF and trailing newlines are trimmed - a compliant TUI
/// treats in-envelope newlines as literal input, and a target without
/// bracketed-paste awareness never sees a trailing CR it could read as a
/// submit. Oversized drafts are truncated at a char boundary (64 KiB,
/// IPC parity). Returns `(normalized, was_truncated)`. Shared with the
/// Launch Pad prompt, which feeds the same PTY prefill path.
pub(crate) fn normalize_composer_text(text: &str) -> (String, bool) {
    let mut t = text.replace("\r\n", "\n").replace('\r', "\n");
    while t.ends_with('\n') {
        t.pop();
    }
    let truncated = t.len() > MAX_COMPOSER_TEXT;
    if truncated {
        let mut cut = MAX_COMPOSER_TEXT;
        while cut > 0 && !t.is_char_boundary(cut) {
            cut -= 1;
        }
        t.truncate(cut);
    }
    (t, truncated)
}

/// Live Composer session owned by `SplitlaneApp` - the source of truth. The
/// target pane renders the pushed [`ComposerSlot`] snapshot.
pub(crate) struct ComposerState {
    pub(crate) input: Entity<TextArea>,
    /// Weak: the pane can close while the user is typing - validation then
    /// degrades to a clean no-op and the overlay closes.
    pub(crate) target: WeakEntity<Pane>,
    /// Broadcast mode toggle: deliver to every ready member of the
    /// active group instead of the single target pane.
    pub(crate) broadcast: bool,
}

/// Render payload pushed into the target [`Pane`]. The pane stays dumb (no
/// access to app state); the closures route user gestures back to
/// `SplitlaneApp` through a weak handle. Re-pushed by
/// [`SplitlaneApp::refresh_composer_slot`] whenever sessions or groups
/// change, so `busy` / `group_label` / `pending_count` track live state.
#[derive(Clone)]
pub(crate) struct ComposerSlot {
    pub(crate) input: Entity<TextArea>,
    pub(crate) broadcast: bool,
    /// The target pane's mapped session is `Thinking`: the state chip warns
    /// that validation will queue instead of delivering (the chip carries the
    /// unified buffering semantics shared with broadcast).
    pub(crate) busy: bool,
    /// Who this goes to - the target surface's own name. The overlay names
    /// its addressee because it is the one place in the app where the user
    /// writes to a session without looking at it.
    pub(crate) addressee: SharedString,
    /// "name · N members" for the active group, `None` when no group is
    /// defined yet.
    pub(crate) group_label: Option<SharedString>,
    /// Number of queued prompts (drives the cancel affordance).
    pub(crate) pending_count: usize,
    pub(crate) dismiss: Rc<dyn Fn(&mut App)>,
    pub(crate) toggle_broadcast: Rc<dyn Fn(&mut App)>,
    pub(crate) cancel_pending: Rc<dyn Fn(&mut App)>,
}

impl SplitlaneApp {
    /// `true` when a session mapped to `surface_id` is generating - the only
    /// state in which delivery must be withheld (a prompt typed into a
    /// generating agent corrupts its stdin). Sessions without a resolved
    /// surface never block anything (they cannot be attributed to a pane).
    pub(crate) fn surface_busy(&self, surface_id: u64) -> bool {
        self.workspaces.iter().any(|ws| {
            ws.agent_sessions
                .values()
                .any(|s| s.surface_id == Some(surface_id) && state_blocks_delivery(&s.state))
        })
    }

    pub(crate) fn handle_open_composer(
        &mut self,
        _: &crate::OpenComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Pane-scoped: refuses while another surface is on screen. The
        // binding's `Terminal` key context makes that visible in the palette
        // and in Settings, instead of the key silently doing nothing.
        if !self.panes_surface_visible() {
            return;
        }
        if self.composer.is_some() {
            self.close_composer(cx);
            return;
        }
        let Some(pane) = self.focused_or_first_pane(window, cx) else {
            return;
        };

        let weak_app = cx.entity().downgrade();
        // The overlay is not a composer and does not call itself
        // one. The function was never "an input field outside the terminal" -
        // it was "write to an agent without going to its pane", which is what
        // makes this app worth having at four sessions and has nothing to do
        // with a rendered conversation.
        let input = cx.new(|cx| TextArea::new("Message the agent \u{2014} Enter sends", cx));
        // Esc closes and keeps the text, so the draft survives the trip - but
        // only back to the pane it was addressed to.
        let target_id = pane.entity_id();
        if let Some((_, draft)) = self
            .composer_draft
            .as_ref()
            .filter(|(pane_id, _)| *pane_id == target_id)
        {
            let draft = draft.clone();
            input.update(cx, |ta, cx| ta.set_value(draft, cx));
        }
        input.update(cx, |ta, _| {
            // Re-entrancy: TextArea callbacks fire synchronously inside the
            // TextArea's own update, so every mutation of app state is
            // deferred (`cx.defer` + weak upgrade) instead of nesting entity
            // updates.
            let w = weak_app.clone();
            // Enter **sends**. It used to pre-fill and leave the
            // Enter to the user, which was the right default while this was a
            // second way to write a message; addressed at one agent's terminal
            // it is a message, and a field that says "goes into the agent's
            // terminal" has to go there. `composer_deliver` still refuses to
            // submit in broadcast - fanning a submit out to N sessions is the
            // multi-target risk broadcast guards against, and the design's overlay
            // is not a broadcast tool.
            ta.on_submit(move |text, _window, cx| {
                let w = w.clone();
                cx.defer(move |cx| {
                    let _ = w.update(cx, |app, cx| app.composer_deliver(text, true, cx));
                });
            });
            let w = weak_app.clone();
            ta.on_submit_immediate(move |text, _window, cx| {
                let w = w.clone();
                cx.defer(move |cx| {
                    let _ = w.update(cx, |app, cx| app.composer_deliver(text, true, cx));
                });
            });
            let w = weak_app.clone();
            ta.on_escape(move |_window, cx| {
                let w = w.clone();
                cx.defer(move |cx| {
                    let _ = w.update(cx, |app, cx| app.close_composer(cx));
                });
            });
            // Every keystroke, because Esc is not the only way this closes: a
            // click outside dismisses it too, and a draft that survived one
            // exit and not the other would be worse than one that never
            // survived at all.
            let w = weak_app.clone();
            ta.on_change(move |text, _cursor, cx| {
                let (w, text) = (w.clone(), text.to_string());
                cx.defer(move |cx| {
                    let _ = w.update(cx, |app, _cx| {
                        app.composer_draft = Some((target_id, text));
                    });
                });
            });
        });
        input.read(cx).focus_handle.clone().focus(window, cx);

        self.composer = Some(ComposerState {
            input,
            target: pane.downgrade(),
            broadcast: false,
        });
        self.refresh_composer_slot(cx);
        cx.notify();
    }

    /// Close the Composer, clear the pushed slot and hand focus back to the
    /// target pane's terminal (via the existing one-shot
    /// `pending_pane_focus`, consumed by `render` which owns a `Window`).
    pub(crate) fn close_composer(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.composer.take() else {
            return;
        };
        if let Some(pane) = state.target.upgrade() {
            pane.update(cx, |p, cx| p.set_composer_slot(None, cx));
            self.pending_pane_focus = Some(pane);
        }
        cx.notify();
    }

    /// Validation path for both Enter (deliver, `submit = false`) and the
    /// explicit `secondary-enter` gesture (`submit = true`). Broadcast mode
    /// never submits, even explicitly.
    pub(crate) fn composer_deliver(&mut self, text: String, submit: bool, cx: &mut Context<Self>) {
        let Some(state) = self.composer.take() else {
            return;
        };
        let broadcast = state.broadcast;
        let Some(pane) = state.target.upgrade() else {
            // Target pane closed while typing - clean no-op,
            // overlay already gone with the pane.
            cx.notify();
            return;
        };
        pane.update(cx, |p, cx| p.set_composer_slot(None, cx));
        self.pending_pane_focus = Some(pane.clone());

        let (text, truncated) = normalize_composer_text(&text);
        if truncated {
            self.show_toast("Prompt truncated to 64 KiB", cx);
        }
        if text.trim().is_empty() {
            // TextArea::submit already no-ops on empty input; belt for the
            // submit-immediate path.
            cx.notify();
            return;
        }

        if broadcast {
            let members = self.live_active_group_members(cx);
            let mut delivered = 0usize;
            let mut queued = 0usize;
            for member in &members {
                let Some(term) = member.read(cx).active_terminal_opt().cloned() else {
                    continue;
                };
                let sid = term.entity_id().as_u64();
                if self.surface_busy(sid) {
                    // Latest-wins: a new broadcast to the same busy pane
                    // REPLACES its buffer.
                    self.broadcast.pending.insert(sid, text.clone());
                    queued += 1;
                } else {
                    term.read(cx).inject_text(&text);
                    delivered += 1;
                }
            }
            self.composer_draft = None;
            self.sync_pending_chips(cx);
            // Never a silent fan-out - transient recap with the
            // specified 4 s hold (longer than the default confirmations).
            self.push_toast(
                format!("Broadcast: {delivered} delivered · {queued} queued"),
                Vec::new(),
                COMPOSER_RECAP_HOLD_MS,
                cx,
            );
        } else {
            let Some(term) = pane.read(cx).active_terminal_opt().cloned() else {
                cx.notify();
                return;
            };
            let sid = term.entity_id().as_u64();
            if self.surface_busy(sid) {
                // The single-pane Composer buffers instead of
                // blocking - same mechanics, same tab indicator as the
                // broadcast buffer. The explicit submit gesture is dropped
                // with the queue (a flush only ever pre-fills).
                self.broadcast.pending.insert(sid, text);
                // Queued counts as delivered: the text is held by the app and
                // flushes on the agent's next idle transition.
                self.composer_draft = None;
                self.sync_pending_chips(cx);
                self.push_toast(
                    "Agent is generating - prompt queued, will pre-fill when it settles".into(),
                    Vec::new(),
                    COMPOSER_RECAP_HOLD_MS,
                    cx,
                );
            } else {
                term.read(cx).inject_text(&text);
                // Cleared here and nowhere earlier: the branches above return
                // without delivering when the target terminal has gone, and a
                // draft cleared on that path is a message the user typed and
                // cannot get back.
                self.composer_draft = None;
                if submit {
                    // Deliver THEN submit. Reuse the IPC's
                    // deferred-CR path so an agent that treats a fresh paste
                    // burst specially cannot swallow the submit.
                    let floor = std::time::Duration::from_millis(
                        self.cached_config.resolved_submit_paste_delay_ms(),
                    );
                    Self::schedule_deferred_submit(&term, floor, cx);
                }
            }
        }
        cx.notify();
    }

    /// Toggle the Composer's broadcast mode. Requires an active
    /// group - otherwise points the user at the picker instead of silently
    /// doing nothing.
    pub(crate) fn toggle_composer_broadcast(&mut self, cx: &mut Context<Self>) {
        if self.composer.is_none() {
            return;
        }
        let has_group = self
            .broadcast
            .active
            .is_some_and(|i| i < self.broadcast.groups.len());
        if !has_group {
            self.show_toast("No broadcast group - open the group picker first", cx);
            return;
        }
        if let Some(state) = &mut self.composer {
            state.broadcast = !state.broadcast;
        }
        self.refresh_composer_slot(cx);
        cx.notify();
    }

    /// Drop every queued prompt (Composer cancel affordance).
    pub(crate) fn cancel_all_pending(&mut self, cx: &mut Context<Self>) {
        if self.broadcast.pending.is_empty() {
            return;
        }
        self.broadcast.pending.clear();
        self.sync_pending_chips(cx);
        self.refresh_composer_slot(cx);
        cx.notify();
    }

    /// Drop the queued prompt of one terminal (pane context-menu cancel).
    pub(crate) fn cancel_pending_for(&mut self, surface_id: u64, cx: &mut Context<Self>) {
        if self.broadcast.pending.remove(&surface_id).is_some() {
            self.sync_pending_chips(cx);
            self.refresh_composer_slot(cx);
            cx.notify();
        }
    }

    /// Recompute the pushed [`ComposerSlot`] from live state. Cheap no-op
    /// when the Composer is closed; closes it when the target pane died.
    pub(crate) fn refresh_composer_slot(&mut self, cx: &mut Context<Self>) {
        let (target, input, broadcast) = match &self.composer {
            Some(s) => (s.target.clone(), s.input.clone(), s.broadcast),
            None => return,
        };
        let Some(pane) = target.upgrade() else {
            self.close_composer(cx);
            return;
        };
        let busy = pane
            .read(cx)
            .active_terminal_opt()
            .is_some_and(|t| self.surface_busy(t.entity_id().as_u64()));
        let group_label = self
            .broadcast
            .active
            .and_then(|i| self.broadcast.groups.get(i))
            .map(|g| {
                let count = self.live_active_group_members(cx).len();
                SharedString::from(format!(
                    "{} · {count} member{}",
                    g.name,
                    if count == 1 { "" } else { "s" }
                ))
            });
        let weak = cx.entity().downgrade();
        let dismiss = Rc::new({
            let w = weak.clone();
            move |cx: &mut App| {
                let _ = w.update(cx, |app, cx| app.close_composer(cx));
            }
        });
        let toggle_broadcast = Rc::new({
            let w = weak.clone();
            move |cx: &mut App| {
                let _ = w.update(cx, |app, cx| app.toggle_composer_broadcast(cx));
            }
        });
        let cancel_pending = Rc::new({
            let w = weak.clone();
            move |cx: &mut App| {
                let _ = w.update(cx, |app, cx| app.cancel_all_pending(cx));
            }
        });
        let addressee = pane
            .read(cx)
            .active_terminal_opt()
            .and_then(|term| term.read(cx).agent_thread_id)
            .and_then(|thread_id| self.thread_by_id(thread_id))
            .map(|thread| {
                crate::project::clean_sidebar_title(&thread.title)
                    .unwrap_or_else(|| thread.title.clone())
            })
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::from("this pane"));
        let slot = ComposerSlot {
            input,
            addressee,
            broadcast,
            busy,
            group_label,
            pending_count: self.broadcast.pending.len(),
            dismiss,
            toggle_broadcast,
            cancel_pending,
        };
        pane.update(cx, |p, cx| p.set_composer_slot(Some(slot), cx));
    }

    /// Flush every queued prompt whose target is no longer generating -
    /// PREFILL ONLY, never a CR. Buffers whose terminal disappeared
    /// are dropped silently. Idempotent and cheap when nothing
    /// is queued; called on every agent-session transition (main thread, so
    /// transition and flush are serialized).
    pub(crate) fn flush_pending_prefill(&mut self, cx: &mut Context<Self>) {
        if self.broadcast.pending.is_empty() {
            return;
        }
        let ids: Vec<u64> = self.broadcast.pending.keys().copied().collect();
        let mut changed = false;
        for sid in ids {
            let Some(term) = find_terminal_by_surface_id(&self.workspaces, sid, cx) else {
                self.broadcast.pending.remove(&sid);
                changed = true;
                continue;
            };
            if self.surface_busy(sid) {
                continue;
            }
            if let Some(text) = self.broadcast.pending.remove(&sid) {
                term.read(cx).inject_text(&text);
                changed = true;
            }
        }
        if changed {
            self.sync_pending_chips(cx);
            cx.notify();
        }
    }

    /// Push the queued-prompt indicator down into the panes (tab chip).
    /// Mirrors `sync_attention`: recomputed idempotently from
    /// the pending-buffer truth.
    pub(crate) fn sync_pending_chips(&self, cx: &mut Context<Self>) {
        for ws in &self.workspaces {
            if let Some(root) = &ws.root {
                for pane in root.collect_leaves() {
                    let subset: HashSet<gpui::EntityId> = pane
                        .read(cx)
                        .terminals()
                        .filter(|t| self.broadcast.pending.contains_key(&t.entity_id().as_u64()))
                        .map(|t| t.entity_id())
                        .collect();
                    pane.update(cx, |p, cx| p.set_pending_prefill(subset, cx));
                }
            }
        }
    }

    /// One hook for every agent-session mutation site (`ai.*` handlers,
    /// auto-clear, surface resolution, stale-PID sweep): flush newly
    /// deliverable buffers, then refresh the Composer's live chip.
    pub(crate) fn agent_sessions_changed(&mut self, cx: &mut Context<Self>) {
        self.flush_pending_prefill(cx);
        self.refresh_composer_slot(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_converts_cr_and_crlf_to_lf() {
        let (out, truncated) = normalize_composer_text("a\r\nb\rc\nd");
        assert_eq!(out, "a\nb\nc\nd");
        assert!(!truncated);
    }

    #[test]
    fn normalize_trims_trailing_newlines_only() {
        // Trailing CR/LF would ride right behind the paste envelope and read
        // as a submit on a non-bracketed-paste-aware target; interior
        // newlines are the whole point of the multi-line Composer.
        let (out, _) = normalize_composer_text("line1\nline2\r\n\n\r");
        assert_eq!(out, "line1\nline2");
    }

    #[test]
    fn normalize_truncates_at_char_boundary() {
        // 64 KiB cap, never splitting a multibyte char.
        let big = "é".repeat(MAX_COMPOSER_TEXT); // 2 bytes per char
        let (out, truncated) = normalize_composer_text(&big);
        assert!(truncated);
        assert!(out.len() <= MAX_COMPOSER_TEXT);
        assert!(out.is_char_boundary(out.len()));
        let (ok, truncated) = normalize_composer_text("short prompt");
        assert_eq!(ok, "short prompt");
        assert!(!truncated);
    }
}
