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

/// `open` counts what leaving would end - the agent sessions with a terminal
/// behind them and the commands running in shells ([`OpenWork::total`]) -
/// `running` the agents mid-turn, and `commands` the commands alone.
///
/// A door that ends the sessions asks first when a turn or a command would
/// end with them: stopping a dev server without a word is the same loss as
/// stopping a turn.
pub(crate) fn leave_plan(
    intent: ExitIntent,
    mode: OnQuit,
    open: usize,
    running: usize,
    commands: usize,
) -> Leave {
    let ending = if running > 0 || commands > 0 {
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
pub(crate) const OPEN_SESSION_WINDOW_SECS: i64 = 6;

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
        .filter(|thread| crate::app::agents_sidebar::is_agent_session(thread))
        .count()
}

/// What is still working when the app is left: the agent sessions with a
/// terminal behind them, and the commands running in shells.
///
/// A command counts for the same reason an agent does. Leaving with only
/// shells open used to end them all, and with them a dev server or a build
/// that had been running for twenty minutes, without a word; a shell at its
/// prompt still counts for nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OpenWork {
    pub(crate) agents: usize,
    pub(crate) commands: usize,
}

impl OpenWork {
    pub(crate) fn total(self) -> usize {
        self.agents + self.commands
    }
}

/// How many commands are running in shells, by the last census that read
/// them. A shell an agent runs in is counted as the agent instead.
pub(crate) fn shell_commands_running<'a>(
    threads: impl Iterator<Item = &'a Thread>,
    now: i64,
) -> usize {
    threads
        .filter(|thread| {
            crate::app::agents_sidebar::is_shell_surface(thread) && !thread.shell_runs_agent
        })
        .filter(|thread| {
            thread
                .commands_read_secs
                .is_some_and(|read| now - read <= OPEN_SESSION_WINDOW_SECS)
        })
        .map(|thread| thread.running_commands.len())
        .sum()
}

/// `3 agents and 2 commands are running` - what the quit card counts.
fn work_clause(open: OpenWork) -> String {
    let agents = match open.agents {
        0 => None,
        1 => Some("1 agent".to_string()),
        n => Some(format!("{n} agents")),
    };
    let commands = match open.commands {
        0 => None,
        1 => Some("1 command".to_string()),
        n => Some(format!("{n} commands")),
    };
    let plural = open.total() != 1;
    let what = match (agents, commands) {
        (Some(agents), Some(commands)) => format!("{agents} and {commands}"),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => "Nothing".to_string(),
    };
    format!("{what} {} running", if plural { "are" } else { "is" })
}

/// `3 agents and 2 commands are running: cargo build, npm run dev.` - the
/// commands named by program and subcommand, three of them, then a count.
/// Agents are counted and not named: their names are in Activity, and in one
/// line with the commands they would be a jumble.
fn work_sentence(open: OpenWork, commands: &[String]) -> String {
    let clause = work_clause(open);
    if commands.is_empty() {
        format!("{clause}.")
    } else {
        format!(
            "{clause}: {}.",
            crate::app::running_sheet::name_list(commands)
        )
    }
}

/// The card that offers to keep the sessions, when they run in the session
/// host. Keeping them then ends the window for real, and with it the only
/// thing that says an agent is waiting - which is the one cost of keeping,
/// so it is named.
pub(crate) fn keep_or_stop_body_in_host(open: OpenWork, commands: &[String]) -> String {
    let mut body = work_sentence(open, commands);
    if open.agents > 0 {
        body.push_str(
            " Kept, they go on with Splitlane closed, and nothing tells you an agent is \
             waiting until you open it again.",
        );
    }
    body
}

/// The same card when the window's own process holds the sessions.
pub(crate) fn keep_or_stop_body(open: OpenWork, commands: &[String]) -> String {
    format!(
        "{} Kept, the window is put away; start Splitlane again to come back.",
        work_sentence(open, commands)
    )
}

/// What was left at work when the app was left without a question, to be
/// said once when the person is back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct KeptNotice {
    pub(crate) agents: usize,
    pub(crate) commands: usize,
    /// The window's process ended too: the work went on in the session host.
    /// Otherwise the window was only put away.
    pub(crate) closed: bool,
}

/// The sentence for a [`KeptNotice`].
pub(crate) fn kept_notice(kept: KeptNotice) -> String {
    let agents = match kept.agents {
        0 => None,
        1 => Some("1 agent session".to_string()),
        n => Some(format!("{n} agent sessions")),
    };
    let commands = match kept.commands {
        0 => None,
        1 => Some("1 command".to_string()),
        n => Some(format!("{n} commands")),
    };
    let what = match (agents, commands) {
        (Some(agents), Some(commands)) => format!("{agents} and {commands}"),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => "nothing".to_string(),
    };
    if kept.closed {
        format!("Kept running while Splitlane was closed: {what}.")
    } else {
        format!(
            "Splitlane stayed open with {what} at work. To stop them too, use Quit and Stop Sessions."
        )
    }
}

/// Where a [`KeptNotice`] waits for the next launch: beside the saved
/// session, because a window that closes cannot show it.
fn kept_notice_path() -> Option<std::path::PathBuf> {
    Some(
        splitlane_config::loader::session_path()?
            .parent()?
            .join("kept-on-quit.json"),
    )
}

/// Leave word for the next launch. Blocking, and only on the way out.
/// Written beside and renamed into place, so a process ended mid-write
/// leaves the old file or none, never half of one.
fn write_kept_notice(kept: KeptNotice) {
    let Some(path) = kept_notice_path() else {
        log::warn!("leaving: no place for the kept notice; the next launch will not say it");
        return;
    };
    let partial = path.with_extension("json.partial");
    let written = serde_json::to_vec(&kept)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&partial, bytes))
        .and_then(|()| std::fs::rename(&partial, &path));
    if let Err(err) = written {
        log::warn!("leaving: could not write {}: {err}", path.display());
    }
}

/// The word the last run left, taken once: the file goes as it is read. One
/// that cannot be read is logged and dropped too - it would read no better
/// on the next launch, and the write above never leaves half a file.
pub(crate) fn take_kept_notice() -> Option<KeptNotice> {
    let path = kept_notice_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    match serde_json::from_slice(&bytes) {
        Ok(kept) => Some(kept),
        Err(err) => {
            log::warn!(
                "the kept notice at {} could not be read: {err}",
                path.display()
            );
            None
        }
    }
}

/// The titles of the agents in the middle of a turn, in the order given.
pub(crate) fn running_agent_titles<'a>(threads: impl Iterator<Item = &'a Thread>) -> Vec<String> {
    threads
        .filter(|thread| thread.status == ThreadStatus::Thinking)
        .map(|thread| thread.title.clone())
        .collect()
}

/// Title, sentence and button for the question asked when the sessions end.
/// Pure, so the words are tested.
pub(crate) fn exit_confirm_copy(
    intent: ExitIntent,
    running: &[String],
    commands: &[String],
) -> (String, String, String) {
    let (title, verb, confirm) = match intent {
        ExitIntent::Quit | ExitIntent::QuitAndStop => ("Quit Splitlane", "Quitting", "Quit anyway"),
        ExitIntent::Update => ("Restart to update", "Restarting", "Restart anyway"),
    };
    // Name what goes, then what actually happens - the rule the delete copy
    // set. What goes is the turn; what stays is the session.
    let turns = match running {
        [] => None,
        [one] => Some(format!(
            "\"{one}\" is running. {verb} stops it mid-turn: the session comes back on the next launch, the turn it is on does not."
        )),
        many => Some(format!(
            "{} agents are running. {verb} stops them mid-turn: each session comes back on the next launch, the turn it is on does not.",
            many.len()
        )),
    };
    let stopped = match commands.len() {
        0 => None,
        1 => Some(format!(
            "{} is running in a shell, and {} stops it.",
            commands[0],
            verb.to_lowercase()
        )),
        _ => Some(format!(
            "{} commands are running: {}. {verb} stops them.",
            commands.len(),
            crate::app::running_sheet::name_list(commands)
        )),
    };
    let body = match (turns, stopped) {
        (Some(turns), Some(stopped)) => format!("{turns} {stopped}"),
        (Some(only), None) | (None, Some(only)) => only,
        // Only reachable when everything ended while the card was up: the
        // reason for asking is gone, and the sentence says so rather than
        // repeating a warning that is no longer true.
        (None, None) => format!("Nothing is running any more. {verb} now loses nothing."),
    };
    (title.to_string(), body, confirm.to_string())
}

/// Whether a door that asks nothing - the Dock's Quit, a logout - ends what
/// the session host holds. The same answer the menu's Quit reaches without
/// a question: `stop` ends everything, and so does leaving with nothing at
/// work, because a shell at its prompt left running where no window shows
/// it is only clutter. Before, this door kept every shell whenever the
/// setting was not `stop`, while the menu ended them.
pub(crate) fn settles_by_stopping(mode: OnQuit, open: usize) -> bool {
    mode == OnQuit::Stop || open == 0
}

impl SplitlaneApp {
    /// The commands at work, as the quit card names them: program and
    /// subcommand, in the rail's order.
    fn running_command_names(&self) -> Vec<String> {
        self.running_sheet_rows(false)
            .rows
            .into_iter()
            .filter(|row| !row.agent)
            .map(|row| row.short)
            .collect()
    }

    fn running_agents(&self) -> Vec<String> {
        running_agent_titles(self.workspaces.iter().flat_map(|ws| ws.threads.iter()))
    }

    fn open_sessions(&self) -> OpenWork {
        let now = crate::project::now_unix_secs();
        let threads = || self.workspaces.iter().flat_map(|ws| ws.threads.iter());
        // A session the host holds with no pane is still ended by a quit
        // that stops the host, so what runs in it counts too.
        let held: Vec<&crate::running_commands::RunningCommand> = if self
            .held_commands_read_secs
            .is_some_and(|read| now - read <= OPEN_SESSION_WINDOW_SECS)
        {
            self.held_commands.values().flatten().collect()
        } else {
            Vec::new()
        };
        let held_agents = held
            .iter()
            .filter(|command| command.agent.is_some())
            .count();
        OpenWork {
            agents: open_agent_sessions(threads(), now) + held_agents,
            commands: shell_commands_running(threads(), now) + held.len() - held_agents,
        }
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
            self.open_sessions().total(),
            self.running_agents().len(),
            self.open_sessions().commands,
        )
    }

    /// The one door out. Leaves at once when nothing would be lost, puts the
    /// window away when the sessions are to be kept, and otherwise asks. A
    /// repeat of the question already on screen changes nothing.
    ///
    /// Takes no `Window` because two of the doors have none (the client-side
    /// title bar's event and the update action's shared entry); the card takes
    /// the keyboard on the next frame through `pending_focus`.
    ///
    /// What runs in the shells is read again first, off the render thread,
    /// so a command started since the last pass is counted. A second press
    /// while that read is out changes nothing either.
    pub(crate) fn request_exit(&mut self, intent: ExitIntent, cx: &mut Context<Self>) {
        if self.exit_census_pending {
            return;
        }
        self.exit_census_pending = true;
        self.refresh_running_commands(cx, move |app, cx| {
            app.exit_census_pending = false;
            app.leave_by(intent, cx);
        });
    }

    fn leave_by(&mut self, intent: ExitIntent, cx: &mut Context<Self>) {
        match self.leave_plan_for(intent) {
            Leave::Now => {
                // Whatever was being asked is moot: a card left up for a
                // different door would outlive the process it asked about,
                // or answer for it.
                self.dismiss_exit_card();
                self.perform_exit(intent, cx);
            }
            Leave::Keep => self.keep_sessions(true, cx),
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
    ///
    /// `unasked` is a keep nobody was asked about - the setting's default.
    /// That one is said afterwards, once: on the next launch when the window
    /// closes, on the window's return when it is only put away.
    fn keep_sessions(&mut self, unasked: bool, cx: &mut Context<Self>) {
        if crate::terminal::held_sessions::enabled() {
            self.leave_sessions_in_host_and_quit(unasked, cx);
        } else {
            self.keep_sessions_and_put_window_away(unasked, cx);
        }
    }

    /// End the window's process and nothing else. The sessions are the
    /// host's; the layout written here records which pane showed which, and
    /// that is how the next window finds them.
    fn leave_sessions_in_host_and_quit(&mut self, unasked: bool, cx: &mut Context<Self>) {
        self.dismiss_exit_card();
        self.save_session_blocking(cx);
        let open = self.open_sessions();
        // Before anything that can take the process down with it.
        if unasked {
            write_kept_notice(KeptNotice {
                agents: open.agents,
                commands: open.commands,
                closed: true,
            });
        }
        self.emit_app_exited_and_flush();
        log::info!(
            "leaving: {} agent session(s) and {} command(s) kept working in the session host",
            open.agents,
            open.commands
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
    /// Such a door asks nothing, so it answers as the menu's Quit would
    /// without asking ([`settles_by_stopping`]): what is still at work is
    /// left for the next window to find, unless the setting is `stop`.
    pub(crate) fn settle_host_sessions_on_quit(&mut self, cx: &mut Context<Self>) {
        use crate::terminal::held_sessions;
        if !held_sessions::enabled() || held_sessions::settled() {
            return;
        }
        // Nothing waits on this door, and nothing is drawn any more, so the
        // shells are read here and now rather than trusted from the last pass.
        self.refresh_running_commands_blocking(cx);
        let open = self.open_sessions();
        // An agent counts by its terminal being alive as well as by the
        // pass's stamp, which a window that has just started has not made
        // yet: a Dock Quit in those seconds must not end every agent.
        let mut live_agents = 0;
        self.each_agent_terminal(cx, &mut |view| {
            if view.terminal.child_pid != 0 && view.terminal.detected_agent.is_some() {
                live_agents += 1;
            }
        });
        let open = open.total().max(live_agents + open.commands);
        let mode = self
            .cached_config
            .on_quit_mode_for(held_sessions::enabled());
        if settles_by_stopping(mode, open) {
            held_sessions::stop_all();
        } else {
            // Nobody was asked, so the next launch says what went on.
            let work = self.open_sessions();
            write_kept_notice(KeptNotice {
                agents: work.agents.max(live_agents),
                commands: work.commands,
                closed: true,
            });
            held_sessions::leave_running();
        }
    }

    /// Leave every session working and take the window off the screen.
    fn keep_sessions_and_put_window_away(&mut self, unasked: bool, cx: &mut Context<Self>) {
        self.dismiss_exit_card();
        // The process may still be ended from outside while nobody is
        // looking - the Dock's Quit, a logout - and that door asks nothing.
        self.save_session(cx);
        let kept = self.open_sessions();
        if unasked {
            self.kept_while_away = Some(KeptNotice {
                agents: kept.agents,
                commands: kept.commands,
                closed: false,
            });
        }
        log::info!(
            "leaving: window put away, {} agent session(s) and {} command(s) kept working",
            kept.agents,
            kept.commands
        );
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

    /// The word the last run left on its way out, said as this one starts:
    /// a window that starts is not one that becomes active again, so the
    /// hook below would wait for a return that never comes.
    pub(crate) fn announce_kept_from_last_run(&mut self, cx: &mut Context<Self>) {
        if self.kept_while_away.is_some_and(|kept| kept.closed) {
            self.note_return_to_kept_sessions(cx);
        }
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
        self.push_toast(
            kept_notice(kept),
            vec![crate::app::notifications::ToastAction::ReviewRunning],
            // Long enough to read a sentence and reach its button.
            crate::app::constants::TOAST_HOLD_MS * 8,
            cx,
        );
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
            self.keep_sessions(false, cx);
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
        let commands = self.running_command_names();
        // `Review…` puts the card away for the Running sheet; leaving the
        // sheet asks again, with the counts as they are by then.
        let review = || -> (String, crate::app::confirm_dialog::ConfirmHandler) {
            (
                "Review…".to_string(),
                std::rc::Rc::new(move |this: &mut Self, window, cx| {
                    this.review_from_exit_card(intent, window, cx);
                }),
            )
        };
        // Counted again on every frame, so a card left up while the last
        // session closes turns into the plain question, not a stale offer.
        if self.pending_exit_offers_keep && self.open_sessions().total() > 0 {
            return crate::app::confirm_dialog::render_confirm_dialog(
                crate::app::confirm_dialog::ConfirmDialog {
                    id: "exit-keep-or-stop",
                    title: "Quit Splitlane?".to_string(),
                    body: if crate::terminal::held_sessions::enabled() {
                        keep_or_stop_body_in_host(self.open_sessions(), &commands)
                    } else {
                        keep_or_stop_body(self.open_sessions(), &commands)
                    },
                    confirm_label: "Stop and quit".to_string(),
                    safe: Some((
                        "Keep running".to_string(),
                        std::rc::Rc::new(|this, _window, cx| this.keep_from_card(cx)),
                    )),
                    focus: Some(self.exit_confirm_focus.clone()),
                    on_cancel: std::rc::Rc::new(|this, _window, cx| this.cancel_exit(cx)),
                    on_confirm: std::rc::Rc::new(|this, _window, cx| this.confirm_exit(cx)),
                    aside: Some(review()),
                    // Neither answer destroys anything: stopped processes
                    // keep their conversations, and red here would teach that
                    // the red button is the usual way out.
                    tone: crate::app::confirm_dialog::ConfirmTone::SafeForward,
                },
                ui,
                cx,
            );
        }
        let (title, body, confirm_label) = exit_confirm_copy(intent, &running, &commands);
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
                aside: (!commands.is_empty() || !running.is_empty()).then(review),
                tone: crate::app::confirm_dialog::ConfirmTone::Danger,
            },
            ui,
            cx,
        )
    }

    /// The quit card's `Review…`: the card goes, the Running sheet comes, and
    /// leaving the sheet without going to a session asks again.
    fn review_from_exit_card(
        &mut self,
        intent: ExitIntent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_exit_card();
        self.running_sheet_return_to_exit = Some(intent);
        self.open_running_sheet(window, cx);
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
        let (title, body, confirm) = exit_confirm_copy(ExitIntent::Quit, &["Fix auth".into()], &[]);
        assert_eq!(title, "Quit Splitlane");
        assert_eq!(confirm, "Quit anyway");
        assert!(body.starts_with("\"Fix auth\" is running."), "{body}");
        assert!(body.contains("the session comes back"), "{body}");
        assert!(!body.contains("cannot be undone"), "{body}");
    }

    #[test]
    fn several_running_agents_are_counted_and_the_update_says_restart() {
        let running = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (title, body, confirm) = exit_confirm_copy(ExitIntent::Update, &running, &[]);
        assert_eq!(title, "Restart to update");
        assert_eq!(confirm, "Restart anyway");
        assert!(
            body.starts_with("3 agents are running. Restarting stops them"),
            "{body}"
        );
    }

    #[test]
    fn the_question_before_stopping_names_the_commands() {
        let commands = vec!["npm run dev".to_string(), "cargo build".to_string()];
        let (_, body, _) = exit_confirm_copy(ExitIntent::QuitAndStop, &[], &commands);
        assert_eq!(
            body,
            "2 commands are running: npm run dev and cargo build. Quitting stops them."
        );
        let (_, body, _) =
            exit_confirm_copy(ExitIntent::Quit, &["Fix auth".into()], &commands[..1]);
        assert!(
            body.ends_with("npm run dev is running in a shell, and quitting stops it."),
            "{body}"
        );
    }

    #[test]
    fn quit_keeps_open_sessions_unless_told_to_stop() {
        use ExitIntent::*;
        // Nothing open: every door simply leaves.
        for mode in [OnQuit::Keep, OnQuit::Ask, OnQuit::Stop] {
            assert_eq!(leave_plan(Quit, mode, 0, 0, 0), Leave::Now);
        }
        // Sessions open: the setting decides, and keeping asks nothing even
        // mid-turn, because nothing is lost.
        assert_eq!(leave_plan(Quit, OnQuit::Keep, 2, 1, 0), Leave::Keep);
        assert_eq!(leave_plan(Quit, OnQuit::Ask, 2, 0, 0), Leave::AskKeepOrStop);
        assert_eq!(leave_plan(Quit, OnQuit::Stop, 2, 0, 0), Leave::Now);
        assert_eq!(leave_plan(Quit, OnQuit::Stop, 2, 1, 0), Leave::AskLoseTurn);
        // A command that would end is asked about like a turn.
        assert_eq!(leave_plan(Quit, OnQuit::Stop, 1, 0, 1), Leave::AskLoseTurn);
    }

    #[test]
    fn the_doors_that_end_the_process_ignore_the_setting() {
        for intent in [ExitIntent::QuitAndStop, ExitIntent::Update] {
            for mode in [OnQuit::Keep, OnQuit::Ask, OnQuit::Stop] {
                assert_eq!(leave_plan(intent, mode, 3, 0, 0), Leave::Now);
                assert_eq!(leave_plan(intent, mode, 3, 2, 0), Leave::AskLoseTurn);
                assert_eq!(leave_plan(intent, mode, 3, 0, 1), Leave::AskLoseTurn);
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
        let body = keep_or_stop_body(agents(3), &[]);
        assert!(body.starts_with("3 agents are running."), "{body}");
        assert!(
            body.contains("start Splitlane again to come back"),
            "{body}"
        );
        assert!(keep_or_stop_body(agents(1), &[]).starts_with("1 agent is running."));
        let in_host = keep_or_stop_body_in_host(agents(2), &[]);
        assert!(
            in_host.starts_with("2 agents are running. Kept,"),
            "{in_host}"
        );
        assert!(in_host.contains("nothing tells you"), "{in_host}");
    }

    fn agents(agents: usize) -> OpenWork {
        OpenWork {
            agents,
            commands: 0,
        }
    }

    #[test]
    fn commands_are_named_beside_the_agents() {
        let work = OpenWork {
            agents: 3,
            commands: 5,
        };
        let names: Vec<String> = ["cargo build", "npm run dev", "python backfill.py", "a", "b"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert!(
            keep_or_stop_body_in_host(work, &names).starts_with(
                "3 agents and 5 commands are running: cargo build, npm run dev, python backfill.py and 2 more."
            ),
            "{}",
            keep_or_stop_body_in_host(work, &names)
        );
        let only = OpenWork {
            agents: 0,
            commands: 2,
        };
        let body = keep_or_stop_body_in_host(only, &names[..2]);
        assert_eq!(body, "2 commands are running: cargo build and npm run dev.");
    }

    #[test]
    fn the_word_after_keeping_says_where_the_work_went() {
        let closed = KeptNotice {
            agents: 3,
            commands: 2,
            closed: true,
        };
        assert_eq!(
            kept_notice(closed),
            "Kept running while Splitlane was closed: 3 agent sessions and 2 commands."
        );
        let away = KeptNotice {
            agents: 2,
            commands: 0,
            closed: false,
        };
        assert_eq!(
            kept_notice(away),
            "Splitlane stayed open with 2 agent sessions at work. To stop them too, use Quit and Stop Sessions."
        );
        let round_trip: KeptNotice =
            serde_json::from_slice(&serde_json::to_vec(&closed).expect("encode")).expect("decode");
        assert_eq!(round_trip, closed);
    }

    /// The case behind it: a window with only shells open was quit while
    /// one of them ran a dev server, and the server went with it.
    #[test]
    fn a_shell_running_a_command_is_open_work() {
        let mut shell = Thread::new_terminal("shell", "/tmp", None);
        shell.agent_seen_secs = Some(1_000);
        shell.commands_read_secs = Some(1_000);
        assert_eq!(shell_commands_running([&shell].into_iter(), 1_004), 0);
        shell.running_commands = vec![crate::running_commands::RunningCommand {
            pid: 42,
            command: "npm run dev".to_string(),
            started_unix: None,
            stopped: false,
            agent: None,
        }];
        assert_eq!(shell_commands_running([&shell].into_iter(), 1_004), 1);
        // A reading from long ago says nothing about now.
        assert_eq!(shell_commands_running([&shell].into_iter(), 1_100), 0);
        // A shell running an agent is counted as the agent.
        shell.shell_runs_agent = true;
        assert_eq!(shell_commands_running([&shell].into_iter(), 1_004), 0);
        // And the plan: commands alone are kept, not ended.
        assert_eq!(
            leave_plan(ExitIntent::Quit, OnQuit::Keep, 1, 0, 1),
            Leave::Keep
        );
    }

    #[test]
    fn a_door_that_asks_nothing_settles_as_the_menu_would() {
        assert!(settles_by_stopping(OnQuit::Keep, 0));
        assert!(settles_by_stopping(OnQuit::Ask, 0));
        assert!(!settles_by_stopping(OnQuit::Keep, 1));
        assert!(!settles_by_stopping(OnQuit::Ask, 2));
        assert!(settles_by_stopping(OnQuit::Stop, 3));
    }

    #[test]
    fn a_card_outliving_its_reason_says_nothing_is_lost() {
        let (_, body, _) = exit_confirm_copy(ExitIntent::Quit, &[], &[]);
        assert_eq!(
            body,
            "Nothing is running any more. Quitting now loses nothing."
        );
    }
}
