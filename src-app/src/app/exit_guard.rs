//! One question before leaving the app ends a turn an agent is in the middle of.
//!
//! This app has **no object you can lose** - that is why closing a pane asks
//! nothing (`docs/internals/design-decisions.md`). Leaving the app is the one
//! exception, and only while an agent is `running`: `agents::parent_guard`
//! stops every process the app started when the app goes, on purpose, so an
//! orphan cannot keep spending tokens with nobody watching. The conversation
//! survives - the next launch brings every agent back with `--resume` in its
//! own pane - but the turn that was executing does not, and nothing said so.
//!
//! So every door out of the process comes through [`SplitlaneApp::request_exit`]:
//! `Quit` (the menu and `⌘Q`), `CloseWindow`, the window's own close button
//! (native and client-side), and a click on the update pill that restarts or
//! hands over to the Windows installer. With no agent running it leaves at
//! once, exactly as before. With one running it asks, naming the sessions.
//!
//! **`running` only.** `waiting for you` is a person's to answer and they can
//! see it; `idle` has nothing in flight; `starting` has sent nothing yet. Only a
//! turn in progress is something the person would lose without knowing.
//!
//! **A second press of the same door does not confirm.** It did, for one
//! draft, and a cross-vendor review caught what that costs: holding `⌘Q`
//! auto-repeats the action, so the repeat answered a question the person had
//! not yet read. The answer is a click, or `enter` on the card - and a held
//! `enter` is ignored for the same reason.
//!
//! **The known gap is one detector pass.** `running` comes from the pass that
//! reads every agent every two seconds (`agent_state_pass`), so a turn begun
//! in the last two seconds may not be `running` yet and quits without a
//! question. `starting` is deliberately not counted to close it: it would ask
//! on every fresh launch, where nothing has been sent.

use gpui::Context;

use crate::SplitlaneApp;
use crate::project::{Thread, ThreadStatus};

/// Why the process is about to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitIntent {
    /// `Quit`, `CloseWindow` and both close buttons.
    Quit,
    /// The update pill's click, on the paths that end this process: the
    /// relaunch into an already installed update, and the Windows installer
    /// hand-off. The background pre-install never comes here - it cannot
    /// restart anything by itself.
    Update,
}

/// The titles of the agents in the middle of a turn, in the order given.
pub(crate) fn running_agent_titles<'a>(threads: impl Iterator<Item = &'a Thread>) -> Vec<String> {
    threads
        .filter(|thread| thread.status == ThreadStatus::Thinking)
        .map(|thread| thread.title.clone())
        .collect()
}

/// Title, sentence and button for the question. Pure, so the words are tested.
pub(crate) fn exit_confirm_copy(
    intent: ExitIntent,
    running: &[String],
) -> (String, String, String) {
    let (title, verb, confirm) = match intent {
        ExitIntent::Quit => ("Quit Splitlane", "Quitting", "Quit anyway"),
        ExitIntent::Update => ("Restart to update", "Restarting", "Restart anyway"),
    };
    // Name what goes, then what actually happens - the rule the delete copy
    // set. What goes is the turn; what stays is the session.
    let body = match running {
        // Only reachable when every agent finished while the card was up: the
        // reason for asking is gone, and the sentence says so rather than
        // repeating a warning that is no longer true.
        [] => format!("No agent is running any more. {verb} now loses nothing."),
        [one] => format!(
            "\"{one}\" is running. {verb} stops it mid-turn: the session comes back on the next launch, the turn it is on does not."
        ),
        many => format!(
            "{} agents are running. {verb} stops them mid-turn: each session comes back on the next launch, the turn it is on does not.",
            many.len()
        ),
    };
    (title.to_string(), body, confirm.to_string())
}

impl SplitlaneApp {
    fn running_agents(&self) -> Vec<String> {
        running_agent_titles(self.workspaces.iter().flat_map(|ws| ws.threads.iter()))
    }

    /// The one door out. Leaves at once when nothing would be lost; otherwise
    /// asks. A repeat of the question already on screen changes nothing.
    ///
    /// Takes no `Window` because two of the doors have none (the client-side
    /// title bar's event and the update action's shared entry); the card takes
    /// the keyboard on the next frame through `pending_focus`.
    pub(crate) fn request_exit(&mut self, intent: ExitIntent, cx: &mut Context<Self>) {
        if self.running_agents().is_empty() {
            // Whatever was being asked is moot: a card left up for a different
            // door would outlive the process it asked about, or answer for it.
            self.dismiss_exit_card();
            self.perform_exit(intent, cx);
            return;
        }
        if self.pending_exit == Some(intent) {
            return;
        }
        self.pending_exit = Some(intent);
        self.pending_focus = Some(self.exit_confirm_focus.clone());
        cx.notify();
    }

    fn dismiss_exit_card(&mut self) {
        self.pending_exit = None;
        if self
            .pending_focus
            .as_ref()
            .is_some_and(|handle| *handle == self.exit_confirm_focus)
        {
            self.pending_focus = None;
        }
    }

    /// Put the card away. Focus goes back to the panes by itself: the card's
    /// handle is a listed focus holder, and the render pass returns the
    /// keyboard from a holder that is no longer on screen.
    pub(crate) fn cancel_exit(&mut self, cx: &mut Context<Self>) {
        if self.pending_exit.is_some() {
            self.dismiss_exit_card();
            cx.notify();
        }
    }

    fn confirm_exit(&mut self, cx: &mut Context<Self>) {
        if let Some(intent) = self.pending_exit.take() {
            self.perform_exit(intent, cx);
        }
    }

    fn perform_exit(&mut self, intent: ExitIntent, cx: &mut Context<Self>) {
        match intent {
            ExitIntent::Quit => {
                self.save_session_blocking(cx);
                // Bounded to 2 s by the client; if PostHog is unreachable the
                // worker detaches and the quit still proceeds.
                self.emit_app_exited_and_flush();
                // Releases the Wayland/X11 backdrop before GPUI tears down its
                // display - wanted on every way out, not only the window's
                // close button, which was the one door that called it.
                #[cfg(target_os = "linux")]
                crate::window_chrome::linux_backdrop::clear_subtle_chrome_material();
                cx.quit();
            }
            ExitIntent::Update => self.kickoff_self_update_install(cx),
        }
    }

    pub(crate) fn render_exit_confirm_dialog(
        &self,
        intent: ExitIntent,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (title, body, confirm_label) = exit_confirm_copy(intent, &self.running_agents());
        crate::app::confirm_dialog::render_confirm_dialog(
            crate::app::confirm_dialog::ConfirmDialog {
                id: "exit-confirm",
                title,
                body,
                confirm_label,
                focus: Some(self.exit_confirm_focus.clone()),
                on_cancel: std::rc::Rc::new(|this, _window, cx| this.cancel_exit(cx)),
                on_confirm: std::rc::Rc::new(|this, _window, cx| this.confirm_exit(cx)),
            },
            ui,
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(title: &str, status: ThreadStatus) -> Thread {
        let mut thread = Thread::new_terminal(title, "/tmp", None);
        thread.status = status;
        thread
    }

    #[test]
    fn only_a_turn_in_progress_counts_as_running() {
        let threads = [
            agent("building", ThreadStatus::Thinking),
            agent("asking", ThreadStatus::WaitingForInput),
            agent("done", ThreadStatus::Idle),
            agent("launching", ThreadStatus::Starting),
            agent("crashed", ThreadStatus::Failed),
            agent("testing", ThreadStatus::Thinking),
        ];
        assert_eq!(
            running_agent_titles(threads.iter()),
            vec!["building".to_string(), "testing".to_string()]
        );
    }

    #[test]
    fn one_running_agent_is_named_and_the_session_is_promised_back() {
        let (title, body, confirm) = exit_confirm_copy(ExitIntent::Quit, &["Fix auth".into()]);
        assert_eq!(title, "Quit Splitlane");
        assert_eq!(confirm, "Quit anyway");
        assert!(body.starts_with("\"Fix auth\" is running."), "{body}");
        assert!(body.contains("the session comes back"), "{body}");
        assert!(!body.contains("cannot be undone"), "{body}");
    }

    #[test]
    fn several_running_agents_are_counted_and_the_update_says_restart() {
        let running = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (title, body, confirm) = exit_confirm_copy(ExitIntent::Update, &running);
        assert_eq!(title, "Restart to update");
        assert_eq!(confirm, "Restart anyway");
        assert!(
            body.starts_with("3 agents are running. Restarting stops them"),
            "{body}"
        );
    }

    #[test]
    fn a_card_outliving_its_reason_says_nothing_is_lost() {
        let (_, body, _) = exit_confirm_copy(ExitIntent::Quit, &[]);
        assert_eq!(
            body,
            "No agent is running any more. Quitting now loses nothing."
        );
    }
}
