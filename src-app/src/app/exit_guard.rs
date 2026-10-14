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
//! **Leaving need not end anything.** `Quit` and the close buttons first ask
//! the `on_quit` setting what to do with the agent sessions that are open.
//! `keep` puts the window away - hidden on macOS, minimised elsewhere, because
//! that is all the toolkit can do there - and the process, with every agent
//! under it, goes on; launching the app again brings the window back
//! (`ipc::start_server`). Nothing is lost, so nothing is asked. `ask` offers
//! the two answers on one card. `stop`, the separate `Quit and Stop Sessions`
//! action, and the restart into an update end the process as before, and it
//! is only on those that the question about a turn in progress remains.
//!
//! This is the whole app staying up, not a process of its own holding the
//! terminals: it does not survive an update or a crash.
//!
//! **Unless the panes run in the session host** (`terminal::held_sessions`).
//! Then the terminals do have a process of their own, and keeping the
//! sessions means what it says: the window's process ends and the sessions
//! go on in the host, to be attached to by the next window. Nothing is put
//! away, because nothing has to stay. Stopping them is then a request to
//! the host, made on the way out, and the restart into an update asks
//! nothing at all: it ends the window and no session. What the person
//! gives up while no window runs is being told that an agent waits, and the
//! card that offers to keep the sessions says so.
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

use splitlane_config::schema::OnQuit;

use crate::SplitlaneApp;
use crate::project::{Thread, ThreadStatus};

/// Which door out was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitIntent {
    /// `Quit`, `CloseWindow` and both close buttons. What it does to the open
    /// sessions is the `on_quit` setting's to say.
    Quit,
    /// `Quit and Stop Sessions`: ends the process whatever the setting says.
    QuitAndStop,
    /// The update pill's click, on the paths that end this process: the
    /// relaunch into an already installed update, and the Windows installer
    /// hand-off. The background pre-install never comes here - it cannot
    /// restart anything by itself.
    Update,
}

/// What a door out does, once the setting and the sessions are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Leave {
    /// End the process at once: nothing would be lost.
    Now,
    /// Put the window away and leave everything working.
    Keep,
    /// Ask whether to keep the sessions or stop them.
    AskKeepOrStop,
    /// The process is ending and a turn is in progress: ask before losing it.
    AskLoseTurn,
}

/// `open` counts the agent sessions with a terminal behind them, `running`
/// the ones mid-turn.
pub(crate) fn leave_plan(intent: ExitIntent, mode: OnQuit, open: usize, running: usize) -> Leave {
    let ending = if running > 0 {
        Leave::AskLoseTurn
    } else {
        Leave::Now
    };
    match (intent, mode) {
        (ExitIntent::QuitAndStop | ExitIntent::Update, _) | (ExitIntent::Quit, OnQuit::Stop) => {
            ending
        }
        // Nothing to keep: a window put away with no agent behind it is an
        // app that will not quit.
        (ExitIntent::Quit, _) if open == 0 => ending,
        (ExitIntent::Quit, OnQuit::Keep) => Leave::Keep,
        (ExitIntent::Quit, OnQuit::Ask) => Leave::AskKeepOrStop,
    }
}

/// How long after the detector last saw a session's terminal the session
/// still counts as open: three of its two-second passes.
const OPEN_SESSION_WINDOW_SECS: i64 = 6;

/// How many agent sessions have a terminal behind them now.
///
/// A shell counts only while an agent runs in it. Leaving with nothing but
/// shells open ends them: what the person was not asked to keep, a window
/// that is gone must not leave running where nobody can see it.
pub(crate) fn open_agent_sessions<'a>(
    threads: impl Iterator<Item = &'a Thread>,
    now: i64,
) -> usize {
    threads
        .filter(|thread| {
            thread
                .agent_seen_secs
                .is_some_and(|seen| now - seen <= OPEN_SESSION_WINDOW_SECS)
        })
        .filter(|thread| {
            !crate::app::agents_sidebar::is_shell_surface(thread) || thread.shell_runs_agent
        })
        .count()
}

/// How many sessions are open and how many of them are mid-turn, as the
/// card that offers to keep them begins.
fn open_sessions_clause(open: usize, running: usize) -> String {
    let sessions = match open {
        1 => "1 agent session is open".to_string(),
        n => format!("{n} agent sessions are open"),
    };
    let mid_turn = match running {
        0 => String::new(),
        1 => ", 1 of them mid-turn".to_string(),
        n => format!(", {n} of them mid-turn"),
    };
    format!("{sessions}{mid_turn}")
}

/// The same card when the sessions run in the session host. Keeping them
/// then ends the window for real, and with it the only thing that says an
/// agent is waiting - which is the one cost of keeping, so it is named.
pub(crate) fn keep_or_stop_body_in_host(open: usize, running: usize) -> String {
    format!(
        "{}. Keep running closes Splitlane and leaves them working; nothing tells you \
         that one is waiting for you until you start Splitlane again. Stop and quit ends \
         them: each session comes back on the next launch, a turn in progress does not.",
        open_sessions_clause(open, running)
    )
}

/// The sentence on the card that offers to keep the sessions.
pub(crate) fn keep_or_stop_body(open: usize, running: usize) -> String {
    let (sessions, mid_turn) = (open_sessions_clause(open, running), "");
    format!(
        "{sessions}{mid_turn}. Keep running puts the window away and leaves them working; \
         start Splitlane again to come back. Stop and quit ends them: each session comes \
         back on the next launch, a turn in progress does not."
    )
}

/// What the person is told, once, on coming back to a window that was put
/// away without a question.
pub(crate) fn kept_notice(kept: usize) -> String {
    let sessions = if kept == 1 {
        "1 agent session".to_string()
    } else {
        format!("{kept} agent sessions")
    };
    format!(
        "Splitlane stayed open with {sessions} working. To stop them too, use \
         Quit and Stop Sessions."
    )
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
        ExitIntent::Quit | ExitIntent::QuitAndStop => ("Quit Splitlane", "Quitting", "Quit anyway"),
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

    fn open_sessions(&self) -> usize {
        open_agent_sessions(
            self.workspaces.iter().flat_map(|ws| ws.threads.iter()),
            crate::project::now_unix_secs(),
        )
    }

    fn leave_plan_for(&self, intent: ExitIntent) -> Leave {
        // The sessions are the host's, and a window that restarts into an
        // update takes none of them with it.
        if intent == ExitIntent::Update && crate::terminal::held_sessions::enabled() {
            return Leave::Now;
        }
        leave_plan(
            intent,
            splitlane_config::loader::load_config()
                .on_quit_mode_for(crate::terminal::held_sessions::enabled()),
            self.open_sessions(),
            self.running_agents().len(),
        )
    }

    /// The one door out. Leaves at once when nothing would be lost, puts the
    /// window away when the sessions are to be kept, and otherwise asks. A
    /// repeat of the question already on screen changes nothing.
    ///
    /// Takes no `Window` because two of the doors have none (the client-side
    /// title bar's event and the update action's shared entry); the card takes
    /// the keyboard on the next frame through `pending_focus`.
    pub(crate) fn request_exit(&mut self, intent: ExitIntent, cx: &mut Context<Self>) {
        match self.leave_plan_for(intent) {
            Leave::Now => {
                // Whatever was being asked is moot: a card left up for a
                // different door would outlive the process it asked about,
                // or answer for it.
                self.dismiss_exit_card();
                self.perform_exit(intent, cx);
            }
            Leave::Keep => self.keep_sessions(cx),
            plan @ (Leave::AskKeepOrStop | Leave::AskLoseTurn) => {
                if self.pending_exit == Some(intent) {
                    return;
                }
                self.pending_exit = Some(intent);
                // Settled when the card opens: the setting is a file, and a
                // card that re-read it on every frame could turn from an
                // offer to keep into a plain "quit anyway" under the person.
                self.pending_exit_offers_keep = plan == Leave::AskKeepOrStop;
                self.pending_focus = Some(self.exit_confirm_focus.clone());
                cx.notify();
            }
        }
    }

    /// Leave every session working: in the session host if that is where
    /// they run, and otherwise in this process with its window put away.
    fn keep_sessions(&mut self, cx: &mut Context<Self>) {
        if crate::terminal::held_sessions::enabled() {
            self.leave_sessions_in_host_and_quit(cx);
        } else {
            self.keep_sessions_and_put_window_away(cx);
        }
    }

    /// End the window's process and nothing else. The sessions are the
    /// host's; the layout written here records which pane showed which, and
    /// that is how the next window finds them.
    fn leave_sessions_in_host_and_quit(&mut self, cx: &mut Context<Self>) {
        self.dismiss_exit_card();
        self.save_session_blocking(cx);
        self.emit_app_exited_and_flush();
        log::info!(
            "leaving: {} agent session(s) kept working in the session host",
            self.open_sessions()
        );
        // Before the panes are torn down: a pane that is dropped ends its
        // session unless it was told the window is only going away.
        crate::terminal::held_sessions::leave_running();
        #[cfg(target_os = "linux")]
        crate::window_chrome::linux_backdrop::clear_subtle_chrome_material();
        cx.quit();
    }

    /// Decide what becomes of the sessions in the session host when the
    /// process is ended by a door that did not come through
    /// [`Self::request_exit`]: the Dock's Quit, a logout. Called as the app
    /// quits, by whichever door; a door that has already decided is left
    /// its decision.
    ///
    /// Such a door asks nothing, so the setting answers: `stop` ends them,
    /// anything else leaves them, and they are found by the next window.
    pub(crate) fn settle_host_sessions_on_quit(&mut self) {
        use crate::terminal::held_sessions;
        if !held_sessions::enabled() || held_sessions::settled() {
            return;
        }
        if self
            .cached_config
            .on_quit_mode_for(held_sessions::enabled())
            == OnQuit::Stop
        {
            held_sessions::stop_all();
        } else {
            held_sessions::leave_running();
        }
    }

    /// Leave every session working and take the window off the screen.
    fn keep_sessions_and_put_window_away(&mut self, cx: &mut Context<Self>) {
        self.dismiss_exit_card();
        // The process may still be ended from outside while nobody is
        // looking - the Dock's Quit, a logout - and that door asks nothing.
        self.save_session(cx);
        let kept = self.open_sessions();
        self.kept_while_away = Some(kept);
        log::info!("leaving: window put away, {kept} agent session(s) kept working");
        #[cfg(target_os = "macos")]
        cx.hide();
        // The toolkit can hide an app only on macOS. A minimised window is
        // the nearest thing elsewhere; deferred because the window this was
        // called from is still being updated.
        #[cfg(not(target_os = "macos"))]
        cx.defer(|cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| window.minimize_window());
            }
        });
        cx.notify();
    }

    /// Say once per run what happened to a window that was put away without
    /// a question. Called when the window becomes active again.
    pub(crate) fn note_return_to_kept_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(kept) = self.kept_while_away.take() else {
            return;
        };
        if std::mem::replace(&mut self.kept_notice_shown, true) {
            return;
        }
        self.show_toast(kept_notice(kept), cx);
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
            self.dismiss_exit_card();
            self.perform_exit(intent, cx);
        }
    }

    /// The card's "Keep running".
    fn keep_from_card(&mut self, cx: &mut Context<Self>) {
        if self.pending_exit.take().is_some() {
            self.keep_sessions(cx);
        }
    }

    fn perform_exit(&mut self, intent: ExitIntent, cx: &mut Context<Self>) {
        match intent {
            ExitIntent::Quit | ExitIntent::QuitAndStop => {
                self.save_session_blocking(cx);
                // Bounded to 2 s by the client; if PostHog is unreachable the
                // worker detaches and the quit still proceeds.
                self.emit_app_exited_and_flush();
                // This door ends the sessions. The ones in the session
                // host do not end with this process, so the host is asked
                // to end everything it holds; a no-op in a window that
                // does not use it.
                crate::terminal::held_sessions::stop_all();
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
        let running = self.running_agents();
        // Counted again on every frame, so a card left up while the last
        // session closes turns into the plain question, not a stale offer.
        if self.pending_exit_offers_keep && self.open_sessions() > 0 {
            return crate::app::confirm_dialog::render_confirm_dialog(
                crate::app::confirm_dialog::ConfirmDialog {
                    id: "exit-keep-or-stop",
                    title: "Quit Splitlane".to_string(),
                    body: if crate::terminal::held_sessions::enabled() {
                        keep_or_stop_body_in_host(self.open_sessions(), running.len())
                    } else {
                        keep_or_stop_body(self.open_sessions(), running.len())
                    },
                    confirm_label: "Stop and quit".to_string(),
                    safe: Some((
                        "Keep running".to_string(),
                        std::rc::Rc::new(|this, _window, cx| this.keep_from_card(cx)),
                    )),
                    focus: Some(self.exit_confirm_focus.clone()),
                    on_cancel: std::rc::Rc::new(|this, _window, cx| this.cancel_exit(cx)),
                    on_confirm: std::rc::Rc::new(|this, _window, cx| this.confirm_exit(cx)),
                },
                ui,
                cx,
            );
        }
        let (title, body, confirm_label) = exit_confirm_copy(intent, &running);
        crate::app::confirm_dialog::render_confirm_dialog(
            crate::app::confirm_dialog::ConfirmDialog {
                id: "exit-confirm",
                title,
                body,
                confirm_label,
                safe: None,
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
        let mut thread = Thread::new_terminal(
            title,
            "/tmp",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
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
    fn quit_keeps_open_sessions_unless_told_to_stop() {
        use ExitIntent::*;
        // Nothing open: every door simply leaves.
        for mode in [OnQuit::Keep, OnQuit::Ask, OnQuit::Stop] {
            assert_eq!(leave_plan(Quit, mode, 0, 0), Leave::Now);
        }
        // Sessions open: the setting decides, and keeping asks nothing even
        // mid-turn, because nothing is lost.
        assert_eq!(leave_plan(Quit, OnQuit::Keep, 2, 1), Leave::Keep);
        assert_eq!(leave_plan(Quit, OnQuit::Ask, 2, 0), Leave::AskKeepOrStop);
        assert_eq!(leave_plan(Quit, OnQuit::Stop, 2, 0), Leave::Now);
        assert_eq!(leave_plan(Quit, OnQuit::Stop, 2, 1), Leave::AskLoseTurn);
    }

    #[test]
    fn the_doors_that_end_the_process_ignore_the_setting() {
        for intent in [ExitIntent::QuitAndStop, ExitIntent::Update] {
            for mode in [OnQuit::Keep, OnQuit::Ask, OnQuit::Stop] {
                assert_eq!(leave_plan(intent, mode, 3, 0), Leave::Now);
                assert_eq!(leave_plan(intent, mode, 3, 2), Leave::AskLoseTurn);
            }
        }
    }

    #[test]
    fn a_session_counts_as_open_while_its_terminal_was_seen_lately() {
        let mut seen = agent("seen", ThreadStatus::Idle);
        seen.agent_seen_secs = Some(1_000);
        let mut gone = agent("gone", ThreadStatus::Idle);
        gone.agent_seen_secs = Some(900);
        let never = agent("never shown", ThreadStatus::Idle);
        let threads = [seen, gone, never];
        assert_eq!(open_agent_sessions(threads.iter(), 1_004), 1);
        assert_eq!(open_agent_sessions(threads.iter(), 1_100), 0);
    }

    /// The case that showed it: a window with one shell open was closed,
    /// and the shell went on in the session host, where nothing shows it.
    #[test]
    fn a_shell_is_an_open_session_only_while_an_agent_runs_in_it() {
        let mut shell = Thread::new_terminal("shell", "/tmp", None);
        shell.agent_seen_secs = Some(1_000);
        assert!(crate::app::agents_sidebar::is_shell_surface(&shell));
        assert_eq!(open_agent_sessions([&shell].into_iter(), 1_004), 0);
        shell.shell_runs_agent = true;
        assert_eq!(open_agent_sessions([&shell].into_iter(), 1_004), 1);
    }

    #[test]
    fn the_offer_to_keep_says_what_each_answer_does() {
        let body = keep_or_stop_body(3, 1);
        assert!(
            body.starts_with("3 agent sessions are open, 1 of them mid-turn."),
            "{body}"
        );
        assert!(
            body.contains("start Splitlane again to come back"),
            "{body}"
        );
        assert!(keep_or_stop_body(1, 0).starts_with("1 agent session is open."));
        let in_host = keep_or_stop_body_in_host(2, 1);
        assert!(
            in_host.starts_with(
                "2 agent sessions are open, 1 of them mid-turn. Keep running closes Splitlane"
            ),
            "{in_host}"
        );
        assert!(in_host.contains("nothing tells you"), "{in_host}");
        assert_eq!(
            kept_notice(2),
            "Splitlane stayed open with 2 agent sessions working. To stop them too, use Quit and Stop Sessions."
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
