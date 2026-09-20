//! Telling the rail what every agent is doing, from the file the agent writes
//! for itself plus the liveness of its process.
//!
//! This is where [`crate::agent_state::classify`] reaches a screen. The rule
//! and its two inputs were built and tested away from the pixels; this module
//! is the pass that feeds them and deposits the answer on
//! [`crate::project::Thread::status`], which the rail's dot, the slot header,
//! the title bar's waiting chip and `⌥⇥` all already read.
//!
//! # Why it exists at all
//!
//! Because the product no longer intercepts permission asks by default: a
//! person answers the agent in the agent's own terminal (a decision taken after
//! two days of measuring what the vendors' hooks can and cannot report reliably).
//! What we owe the user instead is not losing agents from view:
//! three answers per surface - working, stopped, waiting for you - and both
//! signals behind those answers are ones no vendor has to keep giving us. A
//! transcript the CLI writes anyway, and a process tree the OS owns.
//!
//! # Shape
//!
//! Three phases, the same as `pane_header::spawn_model_probe`, and for the same
//! reason: both the tail read and the process walk block, and neither may
//! happen on the render thread.
//!
//! 1. main thread - which surfaces to ask about, and where each one's file and
//!    PTY are;
//! 2. `smol::unblock` - one process-table snapshot, then a bounded tail read
//!    per surface;
//! 3. main thread - write what changed, and repaint **only** if something did.
//!
//! It differs from the model probe in two ways that are the point of it:
//!
//! - **Scope is every agent surface, not the visible ones.** The model probe
//!   fills a badge, so it only asks about what is on screen. The whole value of
//!   a dot in the rail is telling you about the agent you are **not** looking
//!   at, so this asks about every mounted agent surface in every container.
//! - **Cadence is seconds, not a quarter minute.** A badge names a fact that
//!   changes when the user changes their model. This one answers "you are being
//!   waited on", and two seconds is the difference between a rail worth
//!   glancing at and a rail worth ignoring.

use std::time::Duration;

use crate::SplitlaneApp;
use crate::agent_state::{AgentProcess, Worker, classify};
use crate::ai_types::AgentState;

/// Long enough for the first agent surfaces to have mounted their PTYs and for
/// a resumed session's transcript to be on disk. Nothing is lost by waiting -
/// there is no state to report before a turn has started.
const FIRST_DELAY: Duration = Duration::from_secs(3);

/// How often the pass runs.
///
/// The read is already bounded on both axes - a 512 KB tail per surface, one
/// process snapshot per pass however many surfaces there are - so the cost of
/// this cadence is a small constant rather than something that grows with the
/// window. Two seconds is chosen against the human, not the machine: it is
/// under the time it takes to look up from another pane.
const INTERVAL: Duration = Duration::from_secs(2);

/// How long "the turn is over" has to keep being true before a working surface
/// is taken to have finished. See [`confirm_run_end`].
///
/// Measured, not chosen: the gap Claude Code 2.1.270 leaves between a
/// background task ending and the woken main agent writing `busy` again was
/// 1.23 s, 1.24 s and 1.36 s over three runs (14 September 2026). This is a
/// little over twice the longest of them, so a gap that runs long under load
/// is still covered. It is deliberately not tied to [`INTERVAL`]: with a
/// two-second pass a real finish lands on the second pass after the first idle
/// reading - four seconds after that reading, and up to six after the agent
/// actually stopped, since the first reading itself trails the stop by up to a
/// pass.
const RUN_END_CONFIRM: Duration = Duration::from_secs(3);

/// One agent surface to ask about.
struct StateTarget {
    thread_id: u64,
    /// The CLI's own session file **and the reader for it**, when this build
    /// has one for that agent. `None` for the fourteen it does not: those
    /// surfaces are still targets, because the third source below needs no file
    /// at all.
    state_file: Option<crate::agent_sessions::StateFile>,
    /// The pane's PTY child - the user's shell. **Not** the agent, and not
    /// `Thread::agent_pid`; see [`crate::process_tree::agent_under_pty`].
    pty_child: u32,
    /// What this surface's subtree looked like at the end of its last turn,
    /// if it has finished one. `None` makes the worker question answer
    /// [`Worker::Unknown`].
    baseline: Option<crate::process_tree::WorkerBaseline>,
    /// The session uuid Splitlane forced onto this surface's CLI. It is the
    /// guard on the agent's own status file: the file reports the session the
    /// process is really running, and equality is what tells this surface's
    /// file apart from a recycled pid's. `None` where there is no forced id,
    /// which is where [`crate::claude_pid_state`] declines to answer at all.
    session_id: Option<String>,
    /// `TerminalState::output_generation` as of this pass: a monotone count of
    /// PTY-output events, already maintained by **both** VT backends for
    /// `workspace.up`'s readiness poll.
    ///
    /// Two passes' values are the whole of the third source. It advanced, so
    /// bytes came out of that PTY since the last pass, so something in there is
    /// running - a **positive** statement, available for all sixteen agents and
    /// owed to no vendor. Silence is not its opposite: a counter that did not
    /// move says nothing at all, and is never read as a person being waited on.
    output_generation: u64,
    /// The directory this surface is anchored to. The guard on adopting a
    /// session id from the agent's own status file - see
    /// [`crate::claude_pid_state::session_for`] - and what the transcript for
    /// an adopted id is looked for under.
    cwd: String,
    /// Whether this surface's agent is the one whose status file this build
    /// reads. Adoption is asked only of it, for the same reason the status
    /// source is: no other CLI writes the file.
    reads_status_file: bool,
    /// The surface's name, for the trace and nothing else.
    ///
    /// Filled only when [`TRACE`] logging is actually on, because a pass that
    /// nobody is reading should not be cloning strings twice a second. The rail
    /// row is what a person points at when they report a wrong dot, and its
    /// name is the only thing they can quote - a `thread_id` is not on screen
    /// anywhere.
    label: Option<String>,
}

/// The log target the whole detector traces under.
///
/// Its own target rather than the module path, so one variable turns on this
/// and nothing else:
///
/// ```text
/// RUST_LOG=splitlane::agent_state=debug cargo run
/// ```
///
/// It exists because this is the one part of the app whose defects are
/// **intermittent and invisible**: a dot that is missing looks exactly like a
/// session that really is idle, and by the time anyone thinks to look, the
/// moment is gone. There is a one-shot probe next door
/// (`claude_sessions::the_detector_on_a_real_session`), and it answers about
/// one surface at one instant, which is the wrong shape for a fault nobody can
/// reproduce on demand. This says, every two seconds and for every surface,
/// what each source answered and which one decided - so the report becomes a
/// log line instead of a screenshot.
pub(crate) const TRACE: &str = "splitlane::agent_state";

/// What one pass learned about a surface: the state to show, and the baseline
/// to keep for the next pass.
struct StateReading {
    thread_id: u64,
    state: Option<AgentState>,
    /// Echoed back from the target so the deposit phase - which runs on the
    /// main thread and owns the previous values - can compare the two.
    output_generation: u64,
    /// A freshly taken baseline, present only when this pass saw the turn end.
    /// `None` means "keep whatever is already stored" - never "forget it".
    baseline: Option<crate::process_tree::WorkerBaseline>,
    /// A session id the agent's own status file reported for this surface that
    /// is **not** the one Splitlane recorded, and whose transcript exists in
    /// this surface's project. `None` for every ordinary pass.
    ///
    /// A **proposal**, not a decision: see [`crate::claude_pid_state::session_for`]
    /// for what makes it necessary and `apply_agent_states` for why it takes two
    /// passes to act on.
    proposed_session: Option<String>,
}

impl SplitlaneApp {
    /// Start the state pass. Called once, from bootstrap.
    pub(crate) fn spawn_agent_state_pass(&self, cx: &mut gpui::Context<Self>) {
        cx.spawn(
            async |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                smol::Timer::after(FIRST_DELAY).await;
                loop {
                    let targets = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                            let targets = app.agent_state_targets(None, cx);
                            // This is the one place that sees every surface at
                            // once, so it is the one place that can free a
                            // baseline whose surface is gone. A stale baseline
                            // is harmless - its root identity would no longer
                            // match - but it would never be freed.
                            //
                            // Keyed on the surface still **existing**, not on it
                            // being in this pass's targets: a transcript that is
                            // briefly unreadable drops a surface from the target
                            // list for one pass, and forgetting its baseline
                            // there would cost it the worker signal until its
                            // next finished turn for no reason at all.
                            let live: std::collections::HashSet<u64> = app
                                .workspaces
                                .iter()
                                .flat_map(|container| container.threads.iter().map(|t| t.id))
                                .collect();
                            app.worker_baselines.retain(|id, _| live.contains(id));
                            app.pty_flow.retain(|id, _| live.contains(id));
                            app.proposed_sessions.retain(|id, _| live.contains(id));
                            // And the one place that can take `starting` off a
                            // surface this pass cannot even look at.
                            //
                            // `agent_state_targets` skips a surface whose PTY
                            // has not spawned (`pty_child == 0`), and a
                            // restored container that is not the active one is
                            // never rendered, so its views never promote and
                            // its launch commands sit in `pending_input`. The
                            // word then had no source that could ever end it:
                            // shipped, and found on a restored second project
                            // still wearing the ring minutes later.
                            //
                            // Taking it down here rather than teaching the
                            // target list to include those surfaces is the
                            // honest direction: a pane we cannot look at is one
                            // we cannot claim anything about, and `starting` is
                            // a claim. `idle` claims less.
                            let watched: std::collections::HashSet<u64> =
                                targets.iter().map(|target| target.thread_id).collect();
                            let mut withdrew = false;
                            for thread in app
                                .workspaces
                                .iter_mut()
                                .flat_map(|container| container.threads.iter_mut())
                            {
                                withdrew |= withdraw_starting_if_unwatched(thread, &watched);
                            }
                            if withdrew {
                                cx.notify();
                            }
                            targets
                        })
                    });
                    let Ok(targets) = targets else {
                        break;
                    };
                    if !targets.is_empty() {
                        let read = smol::unblock(move || read_states(targets)).await;
                        let applied = cx.update(|cx| {
                            this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                                app.apply_agent_states(read, cx);
                            })
                        });
                        if applied.is_err() {
                            break;
                        }
                    }
                    smol::Timer::after(INTERVAL).await;
                }
            },
        )
        .detach();
    }

    /// Re-read one surface's state now, rather than on the next tick.
    ///
    /// This is the hook's job after this change: it no longer says what the
    /// state **is**, it says that the state has probably just moved. A frame
    /// arriving for a surface the detector owns lands here, the file is read
    /// out of turn, and the answer is still the file's. The hook contour going
    /// away therefore costs latency and nothing else - at worst the next tick
    /// finds the same thing two seconds later.
    pub(crate) fn accelerate_agent_state(&mut self, thread_id: u64, cx: &mut gpui::Context<Self>) {
        // One in flight per surface. `PreToolUse` fires before **every** tool
        // call, so an agent working through a turn asks for a re-read a few
        // times a second, and each one is a 512 KB tail parse of the same file.
        // Without this they pile up, all reading the same bytes and all racing
        // to deposit them. A frame that arrives while a read is in flight loses
        // nothing: that read has not happened yet.
        if !self.agent_state_reading.insert(thread_id) {
            return;
        }
        let targets = self.agent_state_targets(Some(thread_id), cx);
        if targets.is_empty() {
            self.agent_state_reading.remove(&thread_id);
            return;
        }
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let read = smol::unblock(move || read_states(targets)).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                        app.agent_state_reading.remove(&thread_id);
                        app.apply_agent_states(read, cx);
                    })
                });
            },
        )
        .detach();
    }

    /// Every mounted agent surface the detector can read, across every
    /// container - or just `only`, when one frame asked about one surface.
    ///
    /// The boundary used to be the two things the **rule** needs: a state file
    /// this build knows how to read (`agent_sessions::state_file_for` decides
    /// that - Claude Code and Codex today) and a live PTY. That excluded the
    /// other agents from the pass entirely, which was right while every source
    /// needed a file.
    ///
    /// The third source needs no file - only the pane's own output counter - so
    /// the boundary is now **a live PTY plus a CLI bound to the surface**. A
    /// surface with no readable transcript still arrives here; it simply gets
    /// no answer from the first two sources, which is what `transcript: None`
    /// says downstream.
    ///
    /// **`terminal_agent.is_some()` is load-bearing and was not needed before.**
    /// A bare shell is a thread with a PTY like any other, and it used to be
    /// excluded for free, because `state_file_for` answers `None` for one.
    /// Widening the boundary to "a live PTY" let shells in, and the third
    /// source would have put a spinner on a shell row for every `ls` - a
    /// statement about the wrong kind of object, and one that reaches the
    /// attention queue and `⌥⇥` through `Thread::status` like any other. The
    /// rail owes three answers about **agents**.
    fn agent_state_targets(&self, only: Option<u64>, cx: &gpui::App) -> Vec<StateTarget> {
        let mut seen = std::collections::HashSet::new();
        let mut targets = Vec::new();
        for container in &self.workspaces {
            let Some(root) = container.root.as_ref() else {
                continue;
            };
            for pane in root.collect_leaves() {
                for tab in &pane.read(cx).tabs {
                    let Some(view) = tab.as_terminal() else {
                        continue;
                    };
                    let view = view.read(cx);
                    let Some(thread_id) = view.agent_thread_id else {
                        continue;
                    };
                    if only.is_some_and(|wanted| wanted != thread_id) {
                        continue;
                    }
                    if !seen.insert(thread_id) {
                        continue;
                    }
                    let pty_child = view.terminal.child_pid;
                    if pty_child == 0 {
                        continue;
                    }
                    if let Some(thread) = container
                        .threads
                        .iter()
                        .find(|t| t.id == thread_id && t.terminal_agent.is_some())
                    {
                        targets.push(StateTarget {
                            thread_id,
                            state_file: crate::agent_sessions::state_file_for(thread),
                            pty_child,
                            baseline: self.worker_baselines.get(&thread_id).cloned(),
                            session_id: thread.session_id.clone(),
                            output_generation: view.terminal.output_generation,
                            cwd: thread.cwd.clone(),
                            reads_status_file: thread.terminal_agent
                                == Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
                            label: log::log_enabled!(target: TRACE, log::Level::Debug)
                                .then(|| thread.title.clone()),
                        });
                    }
                }
            }
        }
        targets
    }

    /// Deposit one pass's readings, taken at `read_at`.
    ///
    /// `None` for a surface means the detector had nothing to say about it this
    /// time - an unreadable or not-yet-written transcript. Two things follow
    /// from that, and the second is the one worth writing down.
    ///
    /// Ownership of the status goes back to the hook until the file speaks
    /// again. And the detector **withdraws** whatever it last said, because a
    /// claim that somebody is being kept waiting has to be renewed by evidence:
    /// left standing, a `WaitingForInput` written just before a transcript
    /// became unreadable would sit in the rail for ever, with nothing behind
    /// it and no hook frame coming to replace it. Withdrawing means `Idle`,
    /// which is the state that draws nothing and claims nothing - not
    /// `Thinking`, which would claim work.
    ///
    /// `read_at` is when the reading was **taken**, and an older reading never
    /// lands on top of a newer one. Two readers write here - the periodic pass
    /// and the hook's out-of-turn re-read - and they can finish out of order,
    /// which without this would let a stale `WaitingForInput` overwrite the
    /// answer that had already corrected it.
    fn apply_agent_states(
        &mut self,
        (read_at, read): (std::time::Instant, Vec<StateReading>),
        cx: &mut gpui::Context<Self>,
    ) {
        let mut changed = false;
        // Adoption is the one thing this pass writes that has to outlive the
        // process. Everything else here is a status, which the next pass takes
        // again in two seconds; a session id is what a **restart** resumes, and
        // an unsaved one would put the surface back on the orphaned session the
        // `/clear` left behind - the exact state this fixes, restored from disk.
        let mut adopted_any = false;
        // Runs that ended this pass, collected rather than announced in place:
        // the loop holds a borrow of `self.workspaces`, and the notification
        // wants to ask `self` whether the pane is on screen.
        let mut finished_runs: Vec<FinishedRun> = Vec::new();
        for reading in read {
            // A baseline is only ever **replaced**, never cleared from here: a
            // pass that did not see the turn end has learned nothing about the
            // subtree's resting shape, and dropping the stored one would send
            // the worker question back to `Unknown` for no reason.
            if let Some(baseline) = reading.baseline {
                self.worker_baselines.insert(reading.thread_id, baseline);
            }
            let previous_generation = self
                .pty_flow
                .get(&reading.thread_id)
                .map(|flow| flow.generation);
            let mut flow = PtyFlow {
                generation: reading.output_generation,
                ours: false,
            };
            for container in &mut self.workspaces {
                if let Some(thread) = container
                    .threads
                    .iter_mut()
                    .find(|t| t.id == reading.thread_id)
                {
                    // **Two passes have to agree before a surface is
                    // re-bound**, and this is the guard that makes reading a
                    // pid-numbered file safe.
                    //
                    // `<pid>.json` is named by pid alone and nothing ever
                    // removes it, so a hard-killed agent leaves one behind; if
                    // that pid is recycled onto *this* pane's agent, the stale
                    // file names a dead session in the very same directory and
                    // every guard in `session_for` passes. The transcript check
                    // does not help - a dead session's transcript is on disk
                    // too. What does is that the window is **transient**: the
                    // live agent writes its own file about two seconds into its
                    // life (measured 27 August), so a stale reading cannot
                    // survive the next pass, while a real `/clear` reports the
                    // same new id for as long as the session lasts.
                    //
                    // A clock comparison was the other candidate and is refused
                    // for the reason `process_tree::ProcId` states: the start
                    // token is deliberately opaque, because comparing a
                    // process's start against a timestamp of ours needs a
                    // shared clock frame that Linux, macOS and Windows do not
                    // give. Agreement needs no frame at all.
                    //
                    // The cost is one more tick - four seconds after a `/clear`
                    // rather than two - against a surface that has been quietly
                    // wrong since the moment it was cleared.
                    let seen_before = self
                        .proposed_sessions
                        .get(&reading.thread_id)
                        .map(String::as_str);
                    if let Some(proposed) = reading.proposed_session.clone()
                        && seen_before == Some(proposed.as_str())
                        && thread.session_id.as_deref() != Some(proposed.as_str())
                    {
                        log::info!(
                            target: TRACE,
                            "#{} follows session {proposed} (was {:?}); two passes agree",
                            reading.thread_id,
                            thread.session_id,
                        );
                        thread.session_id = Some(proposed);
                        changed = true;
                        adopted_any = true;
                    }
                    let state = confirm_run_end(
                        &mut self.run_end_seen_at,
                        thread,
                        read_at,
                        reading.state.clone(),
                    );
                    changed |= deposit(thread, read_at, state);
                    // After the deposit, not across it: the clock's own
                    // presence says whether this surface was in a run, so the
                    // status before the write is no longer part of the question.
                    finished_runs.extend(run_ended(&mut self.running_since, thread));
                    let was_ours = self
                        .pty_flow
                        .get(&reading.thread_id)
                        .is_some_and(|flow| flow.ours);
                    let (moved, ours) = deposit_pty_flow(
                        thread,
                        previous_generation,
                        reading.output_generation,
                        was_ours,
                    );
                    changed |= moved;
                    flow.ours = ours;
                }
            }
            self.pty_flow.insert(reading.thread_id, flow);
            match reading.proposed_session {
                Some(proposed) => {
                    self.proposed_sessions.insert(reading.thread_id, proposed);
                }
                // A pass that proposes nothing clears the standing proposal, so
                // agreement means two passes **in a row** and not two passes
                // ever.
                None => {
                    self.proposed_sessions.remove(&reading.thread_id);
                }
            }
        }
        // An entry may not outlive the surface it measures. `run_ended` empties
        // the clock on every transition out of a run, which covers every
        // surface the pass still visits - and a surface **deleted while it was
        // running** is never visited again, so its entry would stand for the
        // life of the process. Cheap: this map holds at most one id per
        // surface currently working.
        self.running_since.retain(|thread_id, _| {
            crate::project::find_surface(&self.workspaces, *thread_id).is_some()
        });
        // Same reason, same place: a surface deleted while its finish was
        // being confirmed is never visited again.
        self.run_end_seen_at.retain(|thread_id, _| {
            crate::project::find_surface(&self.workspaces, *thread_id).is_some()
        });
        if adopted_any {
            // Debounced and coalesced by `save_session` itself, and reached
            // only when an id actually moved - which is once per `/clear`, not
            // once per pass.
            self.save_session(cx);
        }
        if changed {
            // The waiting chip and `⌥⇥` are built from `Thread::status`
            // (`app::waiting`), so a state the detector found reaches them the
            // same way a hook-reported one does.
            self.sync_attention(cx);
            cx.notify();
        }
        for run in finished_runs {
            self.announce_finished_run(run, cx);
        }
        // A Claude session alive while the limits read said there was no
        // usable login is what signing in inside a pane looks like from here.
        // Floored inside; a no-op whenever the last read succeeded.
        self.refresh_claude_limits_after_agent_pass(cx);
    }

    /// Tell somebody a run they could not see has ended.
    ///
    /// **The detector notifies for the surfaces it reads**, and the `ai.stop`
    /// handler stands down for exactly those - the same split, and the same
    /// reason, as `Thread::status` itself. The two channels used to have
    /// different sources: the detector owned the dot and the hook owned the
    /// toast, so a surface whose shim had gone quiet showed a correct status
    /// and announced nothing at all.
    fn announce_finished_run(&mut self, run: FinishedRun, cx: &mut gpui::Context<Self>) {
        let Some(thread) = self.thread_by_id(run.thread_id) else {
            return;
        };
        let Some(agent) = thread.terminal_agent else {
            return;
        };
        // A shell is not a run and has nothing to announce.
        if crate::app::agents_sidebar::is_shell_surface(thread) {
            return;
        }
        let title = crate::project::clean_sidebar_title(&thread.title)
            .unwrap_or_else(|| thread.title.clone());
        let seen = self.thread_is_seen(run.thread_id, cx);
        // The **mark** is not bounded by the duration floor and the
        // **notification** is, which is the whole difference between the two
        // channels: a mark on a row costs nobody an interruption, so a four
        // second run that ended out of sight is still news the rail can hold,
        // while a toast about it is the noise that makes people switch the
        // feature off.
        if !seen {
            self.mark_finished_unseen(run.thread_id, cx);
        }
        if !crate::agents::notifications::turn_was_long_enough(Some(run.ran_for)) {
            return;
        }
        crate::app::ipc_handler::fire_turn_end_notification(
            agent,
            &title,
            None,
            &self.cached_config,
            seen,
            cx.background_executor().clone(),
        );
    }
}

/// A run the detector watched end.
pub(crate) struct FinishedRun {
    thread_id: u64,
    /// How long it ran, measured from the first pass that saw it working.
    ///
    /// Never optional: a completion is only reported for a surface this pass
    /// **watched** enter a run, so there is always a clock to read. A run the
    /// detector never saw start is not a run it can say anything about - it
    /// used to report one with no duration, which asserted a completion on the
    /// strength of a single sample.
    ran_for: std::time::Duration,
}

/// Maintain the run clock and report a run that just ended.
///
/// One function for both because they are one rule: a surface is in a run while
/// its status is `Thinking`, and the moment that stops being true is both when
/// the clock is read and when it is thrown away. Keeping them apart is how a
/// map like this leaks entries for surfaces that never come back.
///
/// **A run shorter than the pass's own two seconds is not seen at all**, and
/// that is a limit of sampling rather than a rule: the surface goes idle,
/// working, idle between two readings, and `before` is `Idle` both times. It
/// costs a mark on a run nobody could have been waiting on, which is the same
/// run the notification floor would have silenced anyway.
///
/// Only `Thinking` to `Idle` is a completion. "Waiting for you" is the **middle
/// of a run** - it neither ends it nor stops its clock, and it has a
/// notification of its own; `Failed` ends the run and has one too, and
/// announcing a crash as a finish would be the wrong sentence.
///
/// A completion after a permission ask therefore reads `before ==
/// WaitingForInput`, not `Thinking` - which is why the transition test asks
/// only what the status is **now** and lets the clock's presence carry the
/// rest: an entry exists exactly for a surface that has been in a run.
fn run_ended(
    running_since: &mut std::collections::HashMap<u64, std::time::Instant>,
    thread: &crate::project::Thread,
) -> Option<FinishedRun> {
    use crate::project::ThreadStatus;
    if thread.status == ThreadStatus::Thinking {
        running_since
            .entry(thread.id)
            .or_insert_with(std::time::Instant::now);
        return None;
    }
    // **A permission ask is the middle of a run, not the end of one**, so the
    // clock stands through it. It used to be emptied on every non-`Thinking`
    // status, which measured a run interrupted twice as three separate
    // segments - and three six-second segments of a twenty-four-second run all
    // fall under the floor, so a long run that ended out of sight said nothing.
    //
    // Found by a cross-vendor pass, and the sharpest thing about the finding is
    // that `ai_types::next_turn_started` - the hook path's clock, forty lines of
    // documentation away - already stated the opposite rule in words ("a run,
    // not a turn segment"). Two clocks for one concept, disagreeing, is the
    // defect this project keeps closing.
    if matches!(
        thread.status,
        ThreadStatus::WaitingForInput | ThreadStatus::Starting
    ) {
        return None;
    }
    let started = running_since.remove(&thread.id);
    // `started.is_some()` is the "was in a run" test now that the ask no longer
    // clears it: `before == Thinking` would miss the run that ends straight out
    // of a permission ask, which is the commonest way a long run finishes.
    (started.is_some() && thread.status == ThreadStatus::Idle).then(|| FinishedRun {
        thread_id: thread.id,
        ran_for: started.map_or(std::time::Duration::ZERO, |at| at.elapsed()),
    })
}

/// Hold back a reading that would end a run until it has stayed true for
/// [`RUN_END_CONFIRM`]. Returns the state to deposit.
///
/// **A working agent says "idle" for about a second in the middle of its work,
/// every time a background task ends.** Measured on Claude Code 2.1.270 by
/// logging its own `sessions/<pid>.json`: a background agent keeps the file on
/// `busy` for as long as it runs, and when it finishes the file reads `idle`
/// for 1.2-1.4 s until the main agent, woken by the task's notification, writes
/// `busy` again. The session never stopped working - its TUI shows it working
/// throughout - but a pass that samples inside that window used to read a
/// finished run: the surface was marked `finished`, and a run past the
/// notification floor sent a toast about a session that was still going.
/// With a two-second pass that is roughly every other background task.
///
/// So a run ends on a **span**, not a sample: the first reading that would end
/// it starts a clock and deposits `Thinking` again, and the run ends only once
/// a later reading still says so at least [`RUN_END_CONFIRM`] after the first.
/// Any reading that is not a finish clears the clock. A span rather than "two
/// passes in a row" because the passes are not evenly spaced: a hook frame
/// asks for an out-of-turn re-read (`accelerate_agent_state`), and two readings
/// a few hundred milliseconds apart can both land in the same window.
///
/// Only `Thinking` to `Idle` is held. `WaitingForInput` is a claim that
/// somebody is being kept waiting, and holding it past its truth would put up
/// the one false answer this detector may not give. A reading of `None` is not
/// held either: that is a source going silent, which `deposit` handles as a
/// withdrawal of its own.
///
/// **A turn that ends and another that starts inside the span are one run**,
/// and that is a cost, stated rather than hidden: from here a real finish
/// followed by `busy` within three seconds is indistinguishable from the blip,
/// so the first turn is never announced. The sequences that produce it are a
/// Stop hook that makes the agent continue - which is not a finish anyone is
/// waiting on - and a message the person queued in that session while it ran,
/// which they already know about. Telling those apart from the blip would need
/// a second signal of a new turn; the status file carries none.
///
/// It holds for every source, not only the status file, because the question
/// is the same for all of them - a run does not end on one sample - and the
/// cost, a finish reported a few seconds late, is the same too.
fn confirm_run_end(
    seen_at: &mut std::collections::HashMap<u64, std::time::Instant>,
    thread: &crate::project::Thread,
    read_at: std::time::Instant,
    state: Option<AgentState>,
) -> Option<AgentState> {
    // A reading older than the one already deposited is dropped by `deposit`;
    // it must not start or clear the clock on its way there.
    if thread.detector_read_at.is_some_and(|seen| seen > read_at) {
        return state;
    }
    let ends_a_run = thread.status == crate::project::ThreadStatus::Thinking
        && matches!(state, Some(AgentState::Finished));
    if !ends_a_run {
        seen_at.remove(&thread.id);
        return state;
    }
    let first = *seen_at.entry(thread.id).or_insert(read_at);
    if read_at.saturating_duration_since(first) < RUN_END_CONFIRM {
        log::debug!(
            target: TRACE,
            "#{} reads finished; holding it as working until it stays so for {:?}",
            thread.id,
            RUN_END_CONFIRM,
        );
        return Some(AgentState::Thinking);
    }
    seen_at.remove(&thread.id);
    state
}

/// Take `starting` off a surface this pass has no way to look at. `true` when
/// it moved.
///
/// `starting` is the one word set by the launch rather than by a reading, so it
/// is also the one word with no source of its own to end it. Every other status
/// is somebody's claim and is withdrawn by whoever made it; this one has to be
/// withdrawn by the pass that finds it unattended.
///
/// The direction is deliberate: a pane we cannot look at is a pane we cannot
/// claim anything about, and `idle` claims less than `starting` does. The cost
/// is that a surface whose PTY is a few milliseconds from spawning can lose the
/// word before it is seen - which is a missing word rather than a false one.
fn withdraw_starting_if_unwatched(
    thread: &mut crate::project::Thread,
    watched: &std::collections::HashSet<u64>,
) -> bool {
    if thread.status != crate::project::ThreadStatus::Starting || watched.contains(&thread.id) {
        return false;
    }
    thread.status = crate::project::ThreadStatus::Idle;
    true
}

/// Apply one surface's reading to its record. `true` when something moved.
///
/// A free function over one thread because that is the whole of the rule: the
/// caller's loop only finds which record to hand here, and keeping the rule out
/// of that loop is what lets it be tested without a window.
fn deposit(
    thread: &mut crate::project::Thread,
    read_at: std::time::Instant,
    state: Option<AgentState>,
) -> bool {
    if thread.detector_read_at.is_some_and(|seen| seen > read_at) {
        return false;
    }
    let was_owned = thread.detector_read_at.is_some();
    thread.detector_read_at = state.is_some().then_some(read_at);
    let Some(state) = state else {
        // Withdraw - but only what the detector itself put there. A surface it
        // has never spoken for is the hook's, and a pass that could not read a
        // file has no business clearing what the hook said.
        if was_owned && thread.status != crate::project::ThreadStatus::Idle {
            thread.status = crate::project::ThreadStatus::Idle;
            return true;
        }
        return false;
    };
    let status = crate::project::ThreadStatus::from_agent_state(state);
    // `agent_pid` and `agent_proc_start` are deliberately left alone. They are
    // the stale-PID sweep's bookkeeping about the shim, not a statement about
    // the turn, and clearing them here - as the hook path does when it writes
    // `Idle` - would take the sweep's only handle on a killed agent away every
    // time a turn ended normally.
    if thread.status != status {
        thread.status = status;
        return true;
    }
    false
}

/// What the previous pass saw of one surface's PTY output, and whether the
/// status it is looking at was put there by this source.
#[derive(Clone, Copy)]
pub(crate) struct PtyFlow {
    pub(crate) generation: u64,
    /// `true` when the last thing written to `Thread::status` for this surface
    /// came from here. Only what this source wrote does it ever take back -
    /// the same rule `deposit` follows for the detector, and for the same
    /// reason.
    pub(crate) ours: bool,
}

/// The third source: bytes came out of that PTY, so something in there is
/// running.
///
/// Returns `(status moved, this source owns the status now)`.
///
/// # Why it is ranked last, and how it stays there
///
/// It is the weakest true thing anyone can say about an agent, and the only one
/// available for all sixteen: no vendor has to keep giving it, no file has to
/// exist, and both VT backends already count it for `workspace.up`. What it
/// cannot do is tell *what* is running - the agent, the shell echoing a
/// keystroke, a TUI repainting a clock - so the single word it is allowed is
/// `Thinking`, and it may only say it where nothing better speaks:
///
/// - **the detector outranks it** by `detector_read_at`, which is `Some` for
///   every surface the first two sources answered for;
/// - **the hook outranks it** by [`crate::project::Thread::hook_has_spoken`].
///   That guard is not decoration: the hook writes `Idle` at the end of every
///   turn, and without it this source would contradict that within two seconds
///   and the dot would blink for the whole life of the pane;
/// - **and it never overwrites a standing claim.** It writes only over `Idle`,
///   the state that draws nothing.
///
/// # Silence means nothing, deliberately
///
/// A counter that did not advance is not evidence of anything - a person
/// reading output, an agent waiting on a person and an agent that has finished
/// all look identical from here. So the only thing a still counter does is take
/// back what **this** source put up, and it takes it back to `Idle`, which
/// claims nothing. Reading it as "waiting for you" is the negative-signal trap
/// the roadmap names and refuses.
fn deposit_pty_flow(
    thread: &mut crate::project::Thread,
    previous: Option<u64>,
    current: u64,
    was_ours: bool,
) -> (bool, bool) {
    use crate::project::ThreadStatus;

    // **`starting` comes down here, before the ranking, for all sixteen.**
    // The design scopes the word to "from the launch command until the first
    // output from that pane", and this counter is exactly that output - the
    // one signal no vendor has to keep giving us. It is taken down ahead of
    // the rank check because `starting` is not a claim any of the three
    // sources owns: it is the launch's own, and this is the only source
    // guaranteed to run for every agent. Left behind the check it would
    // outlive its truth on a surface whose hook has spoken once and gone
    // quiet - a restored session whose shim is no longer installed.
    //
    // Reaching here means a second pass has run with something to compare
    // against, so the word goes whether the counter moved or not: an agent
    // that printed nothing is not still starting, and `idle` claims less than
    // `starting` does.
    let better_source = thread.detector_read_at.is_some() || thread.hook_has_spoken;
    if thread.status == ThreadStatus::Starting && previous.is_some() {
        let moved = previous.is_some_and(|previous| current > previous);
        thread.status = if moved && !better_source {
            ThreadStatus::Thinking
        } else {
            ThreadStatus::Idle
        };
        return (true, moved && !better_source);
    }
    if better_source {
        // Something that reports itself speaks for this surface. Stand down -
        // and stop claiming whatever this source last put up, without touching
        // the status, which now belongs to the better source.
        return (false, false);
    }
    // The first pass for a surface has nothing to compare against. A counter is
    // only evidence as a *difference*: its absolute value says how much output
    // a pane has ever produced, which is not a statement about now.
    let Some(previous) = previous else {
        return (false, was_ours);
    };
    if current > previous {
        if thread.status == ThreadStatus::Idle {
            thread.status = ThreadStatus::Thinking;
            return (true, true);
        }
        // Somebody else's claim is standing. Leave it, and do not adopt it.
        return (false, false);
    }
    if was_ours && thread.status == ThreadStatus::Thinking {
        thread.status = ThreadStatus::Idle;
        return (true, false);
    }
    (false, was_ours)
}

/// The blocking half: one process snapshot, then one bounded tail read per
/// surface.
///
/// The snapshot is taken once for the whole pass rather than once per surface,
/// because every platform answers "who are this pid's children" by enumerating
/// - so per-surface asking would repeat the walk for every pane on screen.
fn read_states(targets: Vec<StateTarget>) -> (std::time::Instant, Vec<StateReading>) {
    // Stamped before the reads rather than after, so a slow pass is ordered by
    // the state of the world it saw and not by when it happened to finish.
    let read_at = std::time::Instant::now();
    // **Taken before the transcripts, and that order is load-bearing.** The
    // baseline below is only allowed to be taken when the tail says the turn is
    // over, and the tail is read *after* this. Calls are only ever added to a
    // transcript, so a tail with nothing open means nothing was open at this
    // instant either - which is what stops a running command's worker from
    // being absorbed into the baseline as furniture and hidden for the rest of
    // the session. Swapping these two lines would open exactly that hole.
    let snapshot = crate::process_tree::ProcessSnapshot::capture();
    // The directory this build staged its shim binaries into, which is what
    // identifies our own process in a chain that otherwise has two processes
    // with the same name. Canonicalised because the executable path the kernel
    // reports is resolved (`/tmp` is `/private/tmp` on macOS) and a
    // `starts_with` between an unresolved and a resolved path silently never
    // matches - which would look exactly like "this platform cannot answer".
    let shim_dir = crate::ai_hooks::extract::ensure_binaries_extracted()
        .ok()
        .map(|dir| dir.canonicalize().unwrap_or(dir));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let states = targets
        .into_iter()
        .map(|target| {
            // The agent's real pid, resolved from this pass's snapshot. Both
            // sources below need it, and resolving it here rather than twice is
            // also what makes the status file safe to read: this module never
            // touches a pid that a live process table did not just hand it.
            let agent_pid = match (&snapshot, &shim_dir) {
                (Some(snapshot), Some(shim_dir)) => {
                    crate::process_tree::agent_under_pty(snapshot, target.pty_child, shim_dir)
                }
                _ => None,
            };

            // **First source: what the agent says about itself.**
            //
            // Asked before the transcript, and that order is the point. The
            // rule below infers a wait from a call left open in the file, and
            // on Claude Code 2.1.246 a `Bash` permission ask writes *nothing*
            // to the file until after the person answers - so the rule cannot
            // see the single most common way an agent waits for you. The status
            // file says `waiting` while that prompt is on screen. Measured
            // 27 August.
            //
            // Silence here is not an answer: a missing file, a file with no
            // `status` (an IDE-entrypoint session), a word this build does not
            // know, or a file naming another session all fall through to the
            // rule, which is exactly where this surface was before.
            let named = || target.label.as_deref().unwrap_or("?");

            // **Has this surface's session been renamed under us?** `/clear`
            // mints a new uuid and a new transcript, and from that moment every
            // reader still pointed at the old id is reading a file that will
            // never grow again. Asked before either source, because both of
            // them are what goes wrong.
            //
            // Three things have to agree before anything moves: the pid was
            // resolved under this pane's own PTY child, the status file says it
            // is running in this surface's directory, and the session it names
            // has a transcript in that directory's project. Any one of them
            // missing leaves the surface exactly as it was.
            let reported = agent_pid
                .filter(|_| target.reads_status_file)
                .and_then(|pid| crate::claude_pid_state::session_for(pid, &target.cwd));
            let proposed_session = reported
                .filter(|id| Some(id.as_str()) != target.session_id.as_deref())
                .filter(|id| {
                    // A live session has a transcript. This does not defend
                    // against a **stale** file - a dead session's transcript is
                    // on disk too - which is what the two-pass agreement in
                    // `apply_agent_states` is for; it defends against composing
                    // a path to a file that is not there at all.
                    let exists = crate::claude_sessions::session_file_exists(&target.cwd, id);
                    if !exists {
                        // The refusals are where an invisible fault will live,
                        // so they are as loud as the success. A surface that
                        // silently stays on a dead transcript reports `idle`
                        // for ever with nothing anywhere to say why.
                        log::debug!(
                            target: TRACE,
                            "{} (#{}) reports session {id}, but no transcript for it \
                             under {} - not following",
                            named(),
                            target.thread_id,
                            target.cwd,
                        );
                    }
                    exists
                });
            if let Some(proposed) = proposed_session.as_deref() {
                log::debug!(
                    target: TRACE,
                    "{} (#{}) reports session {proposed}, not {:?} - proposing to follow it",
                    named(),
                    target.thread_id,
                    target.session_id,
                );
            }

            if let Some(pid) = agent_pid
                && let Some(state) =
                    crate::claude_pid_state::state_for(pid, target.session_id.as_deref())
            {
                log::debug!(
                    target: TRACE,
                    "{} (#{}) pty={} agent={pid} -> status file says {state:?}",
                    named(),
                    target.thread_id,
                    target.pty_child,
                );
                // The baseline is still refreshed at the end of a turn, so the
                // rule stays warm and correct for the day this file stops
                // answering - a CLI release that drops the field must degrade
                // to the old behaviour, not to a stale one.
                let baseline = (state == AgentState::Finished)
                    .then(|| {
                        snapshot.as_ref().and_then(|snapshot| {
                            crate::process_tree::WorkerBaseline::take(snapshot, target.pty_child)
                        })
                    })
                    .flatten();
                return StateReading {
                    thread_id: target.thread_id,
                    state: Some(state),
                    output_generation: target.output_generation,
                    baseline,
                    proposed_session,
                };
            }

            // Second source: the rule, over the transcript plus the process.
            //
            // A surface whose agent this build has no reader for stops here.
            // It is not reported wrongly - it is not reported by these two
            // sources at all, and the deposit phase's third source is what
            // speaks for it.
            let Some(state_file) = target.state_file.as_ref() else {
                log::debug!(
                    target: TRACE,
                    "{} (#{}) pty={} agent={agent_pid:?} -> no reader for this agent, \
                     the hook speaks for it",
                    named(),
                    target.thread_id,
                    target.pty_child,
                );
                return StateReading {
                    thread_id: target.thread_id,
                    state: None,
                    output_generation: target.output_generation,
                    baseline: None,
                    proposed_session,
                };
            };
            // Each agent's own reader, over its own file's shape. What comes
            // back is the same `TranscriptProbe` either way - the rule below is
            // agent-neutral, and each reader is what makes it so by stating the
            // execution kind of every call it reports.
            let probe = match state_file {
                crate::agent_sessions::StateFile::Claude(path) => {
                    crate::claude_sessions::probe_state_from_tail(path, now)
                }
                crate::agent_sessions::StateFile::Codex(path) => {
                    crate::codex_state::probe_state_from_tail(path, now)
                }
            };
            let Some(probe) = probe else {
                log::debug!(
                    target: TRACE,
                    "{} (#{}) pty={} agent={agent_pid:?} -> transcript {state_file:?} \
                     unreadable or empty; the detector withdraws what it last said",
                    named(),
                    target.thread_id,
                    target.pty_child,
                );
                return StateReading {
                    thread_id: target.thread_id,
                    state: None,
                    output_generation: target.output_generation,
                    baseline: None,
                    proposed_session,
                };
            };
            let (agent, worker) = match (&snapshot, &shim_dir) {
                (Some(snapshot), Some(_shim_dir)) => {
                    // The shim resolution is kept, but for what it **refuses**
                    // rather than for the pid it returns. It answers `None`
                    // whenever the chain is not the measured shape - no shim,
                    // two of them, or a shim without exactly one child - and
                    // that last case is what a **dead agent** looks like. A
                    // dead agent leaves its transcript frozen with a call still
                    // open, and reporting a person waiting on that is a claim
                    // about a session that is gone.
                    //
                    // That refusal used to be carried as `Worker::Unknown`, and
                    // it did not defend what it was written to defend: only the
                    // `Spawns` arm demands `Absent`, so `Instant` accepted the
                    // `Unknown` and reported waiting anyway. Measured live on
                    // 27 August with a `Write` ask and a SIGKILLed agent.
                    // The fact now travels as itself.
                    if agent_pid.is_none() {
                        (AgentProcess::NotSeen, Worker::Unknown)
                    } else {
                        (
                            AgentProcess::Found,
                            crate::process_tree::worker_against(
                                snapshot,
                                target.baseline.as_ref(),
                                target.pty_child,
                            ),
                        )
                    }
                }
                // Not asked, which the rule reads as no evidence - never as
                // "no worker", which is the reading that would claim a person
                // is being kept waiting.
                _ => (AgentProcess::NotSeen, Worker::Unknown),
            };
            let state = classify(&probe, worker, agent);
            log::debug!(
                target: TRACE,
                "{} (#{}) pty={} agent={agent_pid:?}/{agent:?} worker={worker:?} \
                 calls={} turn={:?} errored={} incomplete={} -> rule says {state:?}",
                named(),
                target.thread_id,
                target.pty_child,
                probe
                    .open_calls
                    .iter()
                    .map(|call| format!("{}@{:?}", call.name, call.age))
                    .collect::<Vec<_>>()
                    .join(","),
                probe.open_turn,
                probe.errored,
                probe.incomplete,
            );
            // The turn is over exactly when the file has nothing in flight, so
            // this is where the subtree's resting shape can be read - and it is
            // re-read at the end of **every** turn rather than once, which is
            // what lets a restarted agent, a respawned MCP server or a new
            // wrapper be absorbed instead of counting as a worker for ever.
            //
            // **That sentence is the condition now.** It used to be spelled
            // `state == Finished`, which was the same thing only by accident:
            // the one way to reach `Finished` was an empty set of open calls.
            // Since the trust horizon started filtering expired calls out of
            // that set, a file frozen with an ancient call reaches `Finished`
            // too - and taking a baseline there would absorb whatever is running
            // under the pane into the resting shape, hiding it as a worker for
            // the rest of the session. That is precisely what the paragraph
            // above exists to prevent, so the condition asks the file directly.
            //
            // So this deliberately does **not** agree with `classify`'s answer:
            // an expired-only probe is `Finished` there and non-empty here. The
            // asymmetry is the point, and it is written down on both sides so
            // that reconciling them reads as the change it would be.
            let baseline = probe
                .nothing_in_flight()
                .then(|| {
                    snapshot.as_ref().and_then(|snapshot| {
                        crate::process_tree::WorkerBaseline::take(snapshot, target.pty_child)
                    })
                })
                .flatten();
            StateReading {
                thread_id: target.thread_id,
                state: Some(state),
                output_generation: target.output_generation,
                baseline,
                proposed_session,
            }
        })
        .collect();
    (read_at, states)
}

#[cfg(test)]
mod tests {
    use super::run_ended;
    use crate::ai_types::AgentState;
    use crate::project::{Thread, ThreadStatus};

    use std::collections::HashSet;

    use super::{
        RUN_END_CONFIRM, confirm_run_end, deposit, deposit_pty_flow, withdraw_starting_if_unwatched,
    };

    fn surface() -> Thread {
        Thread::new_terminal("agent", "/tmp", None)
    }

    /// A surface the pass cannot look at wears no claim.
    ///
    /// `agent_state_targets` skips a PTY that has not spawned, and a restored
    /// container that is not the active one is never rendered, so its views
    /// never promote and their launch commands sit buffered. Nothing in the
    /// three sources could ever end `starting` there - found on a shipped
    /// build, on a second project still wearing the ring minutes after
    /// restore.
    #[test]
    fn starting_is_withdrawn_from_a_surface_the_pass_cannot_see() {
        let mut t = surface();
        t.status = ThreadStatus::Starting;
        assert!(withdraw_starting_if_unwatched(&mut t, &HashSet::new()));
        assert_eq!(t.status, ThreadStatus::Idle);
    }

    /// And kept on one it can: that surface has a source that will end the
    /// word on its own terms, which is the whole of the word's scope.
    #[test]
    fn starting_survives_on_a_surface_the_pass_is_watching() {
        let mut t = surface();
        t.status = ThreadStatus::Starting;
        let watched = HashSet::from([t.id]);
        assert!(!withdraw_starting_if_unwatched(&mut t, &watched));
        assert_eq!(t.status, ThreadStatus::Starting);
    }

    /// It withdraws `starting` and nothing else. A pane out of sight says
    /// nothing about a turn somebody else reported.
    #[test]
    fn the_sweep_touches_no_other_word() {
        for status in [
            ThreadStatus::Thinking,
            ThreadStatus::WaitingForInput,
            ThreadStatus::Failed,
            ThreadStatus::Idle,
        ] {
            let mut t = surface();
            t.status = status;
            assert!(!withdraw_starting_if_unwatched(&mut t, &HashSet::new()));
            assert_eq!(t.status, status);
        }
    }

    /// The design scopes `starting` to "from the launch command until the first
    /// output from that pane". Output arrived, so the word goes.
    #[test]
    fn starting_ends_at_the_first_output() {
        let mut t = surface();
        t.status = ThreadStatus::Starting;
        let (moved, ours) = deposit_pty_flow(&mut t, Some(10), 11, false);
        assert!(moved);
        assert!(ours);
        assert_eq!(t.status, ThreadStatus::Thinking);
    }

    /// And it goes even when nothing came out. An agent that printed nothing is
    /// not still starting, and `idle` claims less than `starting` does.
    ///
    /// Deliberately `Idle` and **not** `Failed`, though a cross-vendor review
    /// argued for it: "no bytes in one pass" is not evidence a launch died -
    /// it is the absence of evidence, and the detector must never claim
    /// anything from a lack of evidence (the negative-signal trap). A PTY
    /// that really exited says so through `ChildExited`, which is a positive
    /// fact and has its own path.
    ///
    /// Without this arm the word outlives its truth.
    #[test]
    fn starting_ends_even_when_nothing_was_printed() {
        let mut t = surface();
        t.status = ThreadStatus::Starting;
        let (moved, ours) = deposit_pty_flow(&mut t, Some(10), 10, false);
        assert!(moved);
        assert!(!ours);
        assert_eq!(t.status, ThreadStatus::Idle);
    }

    /// The first pass has nothing to compare against, so it is not evidence
    /// that the pane has produced anything: the word stays up for it.
    #[test]
    fn starting_survives_the_pass_that_cannot_compare() {
        let mut t = surface();
        t.status = ThreadStatus::Starting;
        let (moved, _) = deposit_pty_flow(&mut t, None, 10, false);
        assert!(!moved);
        assert_eq!(t.status, ThreadStatus::Starting);
    }

    /// `starting` is nobody's claim to keep - it is the launch's - so it comes
    /// down ahead of the ranking. A surface whose hook spoke once and went
    /// quiet (a restored session whose shim is gone) would otherwise wear the
    /// word forever, because this source stands down for it everywhere else.
    #[test]
    fn starting_comes_down_even_where_a_better_source_speaks() {
        let mut t = surface();
        t.status = ThreadStatus::Starting;
        t.hook_has_spoken = true;
        let (moved, ours) = deposit_pty_flow(&mut t, Some(10), 11, false);
        assert!(moved);
        assert!(
            !ours,
            "this source does not adopt a surface a hook speaks for"
        );
        assert_eq!(t.status, ThreadStatus::Idle);
    }

    /// The one thing this source is allowed to say, and the evidence it needs:
    /// the counter moved, so bytes came out of that PTY since the last pass.
    #[test]
    fn output_since_the_last_pass_reads_as_work() {
        let mut t = surface();
        let (moved, ours) = deposit_pty_flow(&mut t, Some(10), 11, false);
        assert!(moved);
        assert!(ours);
        assert_eq!(t.status, ThreadStatus::Thinking);
    }

    /// A still counter is not evidence of anything - a person reading output, an
    /// agent waiting on one, and an agent that has finished all look identical
    /// from here. So it takes back only what this source put up, and takes it
    /// back to the state that draws nothing.
    #[test]
    fn silence_withdraws_only_this_sources_own_claim() {
        let mut t = surface();
        deposit_pty_flow(&mut t, Some(10), 11, false);
        assert_eq!(t.status, ThreadStatus::Thinking);

        let (moved, ours) = deposit_pty_flow(&mut t, Some(11), 11, true);
        assert!(moved);
        assert!(!ours);
        assert_eq!(t.status, ThreadStatus::Idle, "withdrawn, not left standing");
    }

    /// Silence must never be read as a person being kept waiting. That is the
    /// negative-signal trap the roadmap names and refuses, and it is the one
    /// mistake in this file that would cost the rail its credibility.
    #[test]
    fn silence_is_never_read_as_waiting() {
        for previous in [None, Some(0), Some(41)] {
            let mut t = surface();
            deposit_pty_flow(&mut t, previous, 41, false);
            assert_ne!(t.status, ThreadStatus::WaitingForInput, "{previous:?}");
        }
    }

    /// A surface the detector answers for is not this source's to touch: the
    /// detector reads what the agent wrote about itself, and this counts bytes.
    #[test]
    fn the_detector_outranks_it() {
        let mut t = surface();
        t.detector_read_at = Some(std::time::Instant::now());
        let (moved, ours) = deposit_pty_flow(&mut t, Some(10), 99, false);
        assert!(!moved);
        assert!(!ours);
        assert_eq!(t.status, ThreadStatus::Idle);
    }

    /// And so is a surface a hook speaks for - this guard is not decoration.
    /// The hook writes `Idle` at the end of every turn; without it this source
    /// would contradict that within two seconds, for the life of the pane.
    #[test]
    fn a_hook_that_has_spoken_outranks_it_for_good() {
        let mut t = surface();
        t.hook_has_spoken = true;
        t.status = ThreadStatus::Idle;
        let (moved, ours) = deposit_pty_flow(&mut t, Some(10), 99, true);
        assert!(!moved);
        assert!(!ours, "and it stops claiming what it last put up");
        assert_eq!(t.status, ThreadStatus::Idle);
    }

    /// A standing claim from anyone else is left alone. This source writes over
    /// `Idle` - the state that draws nothing - and over nothing else.
    #[test]
    fn it_never_overwrites_a_standing_claim() {
        for standing in [
            ThreadStatus::WaitingForInput,
            ThreadStatus::Failed,
            ThreadStatus::Thinking,
        ] {
            let mut t = surface();
            t.status = standing;
            let (moved, ours) = deposit_pty_flow(&mut t, Some(1), 2, false);
            assert!(!moved, "{standing:?}");
            assert!(!ours, "{standing:?}");
            assert_eq!(t.status, standing);
        }
    }

    /// A counter is evidence only as a difference. Its absolute value says how
    /// much output a pane has ever produced, which is not a claim about now -
    /// so the first pass for a surface says nothing at all.
    #[test]
    fn the_first_pass_for_a_surface_says_nothing() {
        let mut t = surface();
        let (moved, ours) = deposit_pty_flow(&mut t, None, 5_000, false);
        assert!(!moved);
        assert!(!ours);
        assert_eq!(t.status, ThreadStatus::Idle);
    }

    /// The detector taking over mid-life must not leave this source's claim
    /// standing behind it, nor let it be taken back later by a still counter.
    #[test]
    fn a_claim_is_dropped_when_a_better_source_arrives() {
        let mut t = surface();
        deposit_pty_flow(&mut t, Some(1), 2, false);
        assert_eq!(t.status, ThreadStatus::Thinking);

        t.detector_read_at = Some(std::time::Instant::now());
        t.status = ThreadStatus::WaitingForInput;
        let (moved, ours) = deposit_pty_flow(&mut t, Some(2), 2, true);
        assert!(!moved);
        assert!(!ours);
        assert_eq!(
            t.status,
            ThreadStatus::WaitingForInput,
            "the detector's answer stands"
        );
    }

    /// A claim that somebody is being kept waiting has to be renewed by
    /// evidence. If the transcript stops being readable the moment after the
    /// detector wrote `WaitingForInput`, leaving it there would put a dot in
    /// the rail for ever with nothing behind it and no hook frame coming - the
    /// agent that would have sent one is the thing that went away.
    #[test]
    fn a_silent_pass_withdraws_the_claim_it_made() {
        let now = std::time::Instant::now();
        let mut thread = Thread::new_terminal("Claude", "/tmp", None);
        assert!(deposit(&mut thread, now, Some(AgentState::WaitingForInput)));
        assert_eq!(thread.status, ThreadStatus::WaitingForInput);

        let later = now + std::time::Duration::from_secs(2);
        assert!(deposit(&mut thread, later, None));
        assert_eq!(thread.status, ThreadStatus::Idle);
        assert_eq!(
            thread.detector_read_at, None,
            "and the hook gets the surface back"
        );
    }

    /// Withdrawing only takes back what the detector itself put there. A
    /// surface it has never spoken for is the hook's, and a pass that cannot
    /// read its file has no business clearing what the hook said.
    #[test]
    fn a_silent_pass_does_not_clear_what_the_hook_said() {
        let mut thread = Thread::new_terminal("Codex", "/tmp", None);
        thread.status = ThreadStatus::Thinking;
        assert!(!deposit(&mut thread, std::time::Instant::now(), None));
        assert_eq!(thread.status, ThreadStatus::Thinking);
    }

    /// Two readers write here - the periodic pass and the hook's out-of-turn
    /// re-read - and they can finish out of order. An older reading landing on
    /// top of a newer one is how a `WaitingForInput` that has already been
    /// answered gets put back.
    #[test]
    fn an_older_reading_never_lands_on_a_newer_one() {
        let early = std::time::Instant::now();
        let late = early + std::time::Duration::from_millis(50);
        let mut thread = Thread::new_terminal("Claude", "/tmp", None);

        assert!(deposit(&mut thread, late, Some(AgentState::Thinking)));
        assert!(!deposit(
            &mut thread,
            early,
            Some(AgentState::WaitingForInput)
        ));
        assert_eq!(thread.status, ThreadStatus::Thinking);
        // A stale silence must not withdraw a fresh reading either.
        assert!(!deposit(&mut thread, early, None));
        assert_eq!(thread.status, ThreadStatus::Thinking);
        assert_eq!(thread.detector_read_at, Some(late));
    }
    #[test]
    fn a_run_is_clocked_from_the_first_pass_that_sees_it_working() {
        let mut clock = std::collections::HashMap::new();
        let mut thread = Thread::new_terminal("Claude", "/tmp", None);

        // Enters a run: the clock starts and nothing is announced.
        thread.status = ThreadStatus::Thinking;
        assert!(run_ended(&mut clock, &thread).is_none());
        assert!(clock.contains_key(&thread.id));

        // Still working two seconds later: the original stamp survives, so the
        // run is measured from where it began and not from the last pass.
        let first = clock[&thread.id];
        assert!(run_ended(&mut clock, &thread).is_none());
        assert_eq!(clock[&thread.id], first);

        // Ends: announced once, and the clock is emptied in the same breath.
        thread.status = ThreadStatus::Idle;
        let run = run_ended(&mut clock, &thread).expect("a run that ended is a run to announce");
        assert_eq!(run.thread_id, thread.id);
        assert!(
            run.ran_for < std::time::Duration::from_secs(5),
            "just measured"
        );
        assert!(clock.is_empty(), "no entry outlives the run it measures");

        // And it is announced once: the next pass sees Idle to Idle.
        assert!(run_ended(&mut clock, &thread).is_none());
    }

    #[test]
    fn waiting_for_you_and_failed_are_not_completions() {
        let mut clock = std::collections::HashMap::new();
        let mut thread = Thread::new_terminal("Claude", "/tmp", None);
        thread.status = ThreadStatus::Thinking;
        run_ended(&mut clock, &thread);

        // A permission ask is the middle of a run and has its own notification.
        thread.status = ThreadStatus::WaitingForInput;
        assert!(run_ended(&mut clock, &thread).is_none());

        thread.status = ThreadStatus::Thinking;
        run_ended(&mut clock, &thread);
        // A crash has one too, and calling it a finish is the wrong sentence.
        thread.status = ThreadStatus::Failed;
        assert!(run_ended(&mut clock, &thread).is_none());
        assert!(clock.is_empty(), "and the clock is still emptied");
    }

    /// A run already in flight when the detector first reaches it has no start,
    /// and that is reported as `None` rather than as a zero-length run - the
    /// difference between "it was quick" and "we do not know".
    /// A run the pass never watched start is not a run it can report.
    ///
    /// It used to be reported with no duration, which asserted a completion on
    /// the strength of one sample - and, with the floor reading "unknown" as
    /// "announce it", turned every first sighting of an idle surface into a
    /// notification. The clock's presence is the evidence.
    #[test]
    fn a_run_the_pass_never_saw_start_is_not_reported() {
        let mut clock = std::collections::HashMap::new();
        let mut thread = Thread::new_terminal("Claude", "/tmp", None);
        thread.status = ThreadStatus::Idle;
        assert!(run_ended(&mut clock, &thread).is_none());
    }

    /// The clock stands through a permission ask, so a run interrupted twice is
    /// still one run. Three six-second segments of a twenty-four-second run all
    /// fall under the floor; the run does not.
    #[test]
    fn a_permission_ask_does_not_restart_the_run_clock() {
        let mut clock = std::collections::HashMap::new();
        let mut thread = Thread::new_terminal("Claude", "/tmp", None);
        thread.status = ThreadStatus::Thinking;
        run_ended(&mut clock, &thread);
        let started = clock[&thread.id];

        thread.status = ThreadStatus::WaitingForInput;
        assert!(run_ended(&mut clock, &thread).is_none(), "not a completion");
        assert_eq!(clock.get(&thread.id), Some(&started), "and not a reset");

        thread.status = ThreadStatus::Thinking;
        run_ended(&mut clock, &thread);
        assert_eq!(clock[&thread.id], started, "the run is still the same run");

        // And it ends straight out of the ask on the next reading, which is the
        // commonest way a long run finishes.
        thread.status = ThreadStatus::WaitingForInput;
        run_ended(&mut clock, &thread);
        thread.status = ThreadStatus::Idle;
        assert!(run_ended(&mut clock, &thread).is_some());
        assert!(clock.is_empty());
    }

    /// One pass of the real pipeline over one reading: hold, deposit, clock.
    /// Returns whether that pass announced a finished run.
    fn pass(
        seen_at: &mut std::collections::HashMap<u64, std::time::Instant>,
        clock: &mut std::collections::HashMap<u64, std::time::Instant>,
        thread: &mut Thread,
        read_at: std::time::Instant,
        state: AgentState,
    ) -> bool {
        let state = confirm_run_end(seen_at, thread, read_at, Some(state));
        deposit(thread, read_at, state);
        run_ended(clock, thread).is_some()
    }

    /// The case that was reported: a session still working, whose status file
    /// said `idle` for the second between a background task ending and the
    /// main agent waking up. One pass sampled that second. Nothing may be
    /// announced, and the surface stays `running` throughout.
    #[test]
    fn an_idle_blip_inside_a_run_is_not_a_finish() {
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);
        let (mut seen_at, mut clock) = (Default::default(), Default::default());
        let mut thread = surface();

        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(0),
            AgentState::Thinking
        ));
        // Sampled inside the window - and an out-of-turn re-read lands in it too.
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(2_000),
            AgentState::Finished
        ));
        assert_eq!(thread.status, ThreadStatus::Thinking);
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(2_300),
            AgentState::Finished
        ));
        assert_eq!(thread.status, ThreadStatus::Thinking);
        // The main agent is back.
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(4_000),
            AgentState::Thinking
        ));
        assert!(
            seen_at.is_empty(),
            "working again clears the pending finish"
        );
        // A later blip starts from nothing rather than from the old stamp.
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(9_000),
            AgentState::Finished
        ));
        assert_eq!(thread.status, ThreadStatus::Thinking);
    }

    /// A real finish still arrives, once it has stayed a finish for the span -
    /// and it is announced once, as the same run.
    #[test]
    fn a_finish_that_holds_ends_the_run() {
        let t0 = std::time::Instant::now();
        let (mut seen_at, mut clock) = (Default::default(), Default::default());
        let mut thread = surface();

        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            t0,
            AgentState::Thinking
        ));
        let first_idle = t0 + std::time::Duration::from_secs(2);
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            first_idle,
            AgentState::Finished
        ));
        assert!(pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            first_idle + RUN_END_CONFIRM,
            AgentState::Finished
        ));
        assert_eq!(thread.status, ThreadStatus::Idle);
        assert!(seen_at.is_empty());
        let later = first_idle + RUN_END_CONFIRM * 2;
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            later,
            AgentState::Finished
        ));
    }

    /// A reading that `deposit` will drop as older than the one already there
    /// must not touch the hold on its way: an out-of-turn re-read that took the
    /// `busy` before the blip, and finished after the pass that read the blip,
    /// would otherwise clear the clock and restart the span from nothing.
    #[test]
    fn a_stale_reading_does_not_touch_a_pending_finish() {
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);
        let (mut seen_at, mut clock) = (Default::default(), Default::default());
        let mut thread = surface();

        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(0),
            AgentState::Thinking
        ));
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(2_000),
            AgentState::Finished
        ));
        // Taken at 1.9 s, arriving now.
        assert!(!pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(1_900),
            AgentState::Thinking
        ));
        assert_eq!(
            seen_at.get(&thread.id),
            Some(&at(2_000)),
            "the clock stands"
        );
        assert!(pass(
            &mut seen_at,
            &mut clock,
            &mut thread,
            at(2_000) + RUN_END_CONFIRM,
            AgentState::Finished
        ));
    }

    /// Only a run's end is held. A permission ask goes up at once, and an idle
    /// surface reading idle has nothing to confirm.
    #[test]
    fn only_the_end_of_a_run_is_held() {
        let now = std::time::Instant::now();
        let mut seen_at = std::collections::HashMap::new();

        let mut thread = surface();
        thread.status = ThreadStatus::Thinking;
        assert_eq!(
            confirm_run_end(
                &mut seen_at,
                &thread,
                now,
                Some(AgentState::WaitingForInput)
            ),
            Some(AgentState::WaitingForInput)
        );

        let mut idle = surface();
        idle.status = ThreadStatus::Idle;
        assert_eq!(
            confirm_run_end(&mut seen_at, &idle, now, Some(AgentState::Finished)),
            Some(AgentState::Finished)
        );

        // And silence is `deposit`'s to handle, not a finish to hold.
        assert_eq!(confirm_run_end(&mut seen_at, &thread, now, None), None);
        assert!(seen_at.is_empty());
    }
}
