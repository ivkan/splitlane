//! Which shells are running a command, read on the detector's pass.
//!
//! The reading itself is [`crate::running_commands`]; this is where it is
//! scheduled and where its answer lands. Every pass reads every shell surface
//! that has a terminal and no agent in it, and every session the session host
//! holds without a pane, from one process table.
//!
//! Leaving the app asks again before it decides, rather than trusting the
//! last pass: a command started in the two seconds since would otherwise be
//! ended without a word, and that is the one mistake the question at quit is
//! there to prevent.
//!
//! A read can hang: on Linux, reading the command line of a process stuck in
//! uninterruptible sleep blocks until it wakes. So one census runs at a time,
//! a pass that finds one still out skips its turn rather than queueing
//! another thread behind it, and a door out waits for it only so long before
//! deciding on what it already has.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A census is out. Set and cleared by [`census`] itself.
static CENSUS_OUT: AtomicBool = AtomicBool::new(false);

/// How long a door out waits for a fresh census before deciding without it.
const EXIT_READ_WAIT: Duration = Duration::from_millis(1500);

/// How long a detector pass waits for its census. A hung read must not hold
/// up the agents' reading on the next beat.
const PASS_READ_WAIT: Duration = Duration::from_secs(5);

use crate::SplitlaneApp;
use crate::running_commands::{RunningCommand, commands_under};

/// What one census is asked to read.
#[derive(Default)]
pub(crate) struct CensusRoots {
    /// A shell surface's id and its PTY child.
    surfaces: Vec<(u64, u32)>,
    /// A held session's id and its process.
    held: Vec<(String, u32)>,
}

impl CensusRoots {
    fn is_empty(&self) -> bool {
        self.surfaces.is_empty() && self.held.is_empty()
    }
}

/// What one census found.
pub(crate) struct Census {
    read_secs: i64,
    surfaces: HashMap<u64, Vec<RunningCommand>>,
    held: HashMap<String, Vec<RunningCommand>>,
}

/// The blocking half: one process table, then each root's commands. `None`
/// when another census is still out.
fn census(roots: CensusRoots) -> Option<Census> {
    if CENSUS_OUT.swap(true, Ordering::SeqCst) {
        return None;
    }
    let found = census_now(roots);
    CENSUS_OUT.store(false, Ordering::SeqCst);
    Some(found)
}

fn census_now(roots: CensusRoots) -> Census {
    let binaries: Vec<&'static str> = crate::agent_launcher::TerminalAgent::ALL
        .iter()
        .map(|agent| agent.binary())
        .collect();
    let read = |snapshot: Option<&crate::process_tree::ProcessSnapshot>, pid: u32| {
        let mut found = commands_under(snapshot, pid);
        for command in &mut found {
            command.agent = crate::running_commands::agent_of(&command.command, &binaries);
        }
        found
    };
    let read_secs = crate::project::now_unix_secs();
    // Without a table each shell's foreground job is still read; see
    // `commands_under`.
    let snapshot = crate::process_tree::ProcessSnapshot::capture();
    Census {
        read_secs,
        surfaces: roots
            .surfaces
            .into_iter()
            .map(|(thread_id, pid)| (thread_id, read(snapshot.as_ref(), pid)))
            .collect(),
        held: roots
            .held
            .into_iter()
            .map(|(id, pid)| (id, read(snapshot.as_ref(), pid)))
            .collect(),
    }
}

impl SplitlaneApp {
    /// The shell surfaces with a live terminal and no agent, and the held
    /// sessions no pane shows.
    pub(crate) fn census_roots(&self, cx: &gpui::App) -> CensusRoots {
        let shells: std::collections::HashSet<u64> = self
            .workspaces
            .iter()
            .flat_map(|container| container.threads.iter())
            .filter(|thread| {
                crate::app::agents_sidebar::is_shell_surface(thread) && !thread.shell_runs_agent
            })
            .map(|thread| thread.id)
            .collect();
        let mut roots = CensusRoots::default();
        self.each_agent_terminal(cx, &mut |view| {
            let Some(thread_id) = view.agent_thread_id else {
                return;
            };
            if shells.contains(&thread_id)
                && view.terminal.child_pid != 0
                && view.terminal.detected_agent.is_none()
                && !roots.surfaces.iter().any(|(id, _)| *id == thread_id)
            {
                roots.surfaces.push((thread_id, view.terminal.child_pid));
            }
        });
        roots.held = self
            .held_unshown
            .iter()
            .filter(|held| held.pid != 0)
            .map(|held| (held.id.clone(), held.pid))
            .collect();
        roots
    }

    /// Put a census's answer on the surfaces it read, and clear it from the
    /// ones it did not: a surface that now runs an agent, or lost its
    /// terminal, has no commands of its own to report.
    pub(crate) fn apply_census(&mut self, census: Census, cx: &mut gpui::Context<Self>) {
        let mut changed = false;
        for thread in self
            .workspaces
            .iter_mut()
            .flat_map(|container| container.threads.iter_mut())
        {
            let found = census.surfaces.get(&thread.id).cloned().unwrap_or_default();
            if thread.running_commands != found {
                thread.running_commands = found;
                changed = true;
            }
            thread.commands_read_secs = census
                .surfaces
                .contains_key(&thread.id)
                .then_some(census.read_secs);
        }
        if self.held_commands != census.held {
            self.held_commands = census.held;
            changed = true;
        }
        self.held_commands_read_secs = Some(census.read_secs);
        if changed {
            cx.notify();
        }
    }

    /// Read now, off the render thread, then run `then` with the answer in
    /// place - or with the last one, if the read is not back within
    /// [`EXIT_READ_WAIT`] or another census is still out.
    pub(crate) fn refresh_running_commands(
        &mut self,
        cx: &mut gpui::Context<Self>,
        then: impl FnOnce(&mut Self, &mut gpui::Context<Self>) + 'static,
    ) {
        let roots = self.census_roots(cx);
        if roots.is_empty() {
            let empty = Census {
                read_secs: crate::project::now_unix_secs(),
                surfaces: HashMap::new(),
                held: HashMap::new(),
            };
            self.apply_census(empty, cx);
            then(self, cx);
            return;
        }
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let read = smol::unblock(move || census(roots));
                let timeout = async {
                    smol::Timer::after(EXIT_READ_WAIT).await;
                    None
                };
                let found = smol::future::or(read, timeout).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                        match found {
                            Some(found) => app.apply_census(found, cx),
                            None => log::warn!(
                                "leaving: the shells could not be read in time; deciding on the last reading"
                            ),
                        }
                        then(app, cx);
                    })
                });
            },
        )
        .detach();
    }

    /// Read now, on this thread. Only for the moment the process is ending
    /// through a door that cannot wait - the Dock's Quit, a logout - where
    /// nothing is drawn any more and deciding on a two-second-old reading
    /// could end a command started since. A census still out is not waited
    /// for: the last reading stands.
    pub(crate) fn refresh_running_commands_blocking(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(found) = census(self.census_roots(cx)) {
            self.apply_census(found, cx);
        }
    }

    /// The census for one detector pass.
    pub(crate) async fn run_census_pass(
        this: &gpui::WeakEntity<Self>,
        cx: &mut gpui::AsyncApp,
    ) -> bool {
        let Ok(roots) = cx.update(|cx| this.update(cx, |app: &mut Self, cx| app.census_roots(cx)))
        else {
            return false;
        };
        // A census still out from before is left to finish; this pass
        // skips its turn.
        let read = smol::unblock(move || census(roots));
        let timeout = async {
            smol::Timer::after(PASS_READ_WAIT).await;
            None
        };
        let Some(found) = smol::future::or(read, timeout).await else {
            return true;
        };
        cx.update(|cx| {
            this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                app.apply_census(found, cx);
            })
        })
        .is_ok()
    }
}
