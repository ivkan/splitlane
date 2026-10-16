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

use std::collections::HashMap;

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

/// The blocking half: one process table, then each root's commands.
fn census(roots: CensusRoots) -> Census {
    let binaries: Vec<&'static str> = crate::agent_launcher::TerminalAgent::ALL
        .iter()
        .map(|agent| agent.binary())
        .collect();
    let read = |snapshot: &crate::process_tree::ProcessSnapshot, pid: u32| {
        let mut found = commands_under(snapshot, pid);
        for command in &mut found {
            command.agent = crate::running_commands::agent_of(&command.command, &binaries);
        }
        found
    };
    let read_secs = crate::project::now_unix_secs();
    let Some(snapshot) = crate::process_tree::ProcessSnapshot::capture() else {
        return Census {
            read_secs,
            surfaces: HashMap::new(),
            held: HashMap::new(),
        };
    };
    Census {
        read_secs,
        surfaces: roots
            .surfaces
            .into_iter()
            .map(|(thread_id, pid)| (thread_id, read(&snapshot, pid)))
            .collect(),
        held: roots
            .held
            .into_iter()
            .map(|(id, pid)| (id, read(&snapshot, pid)))
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
    /// place.
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
                let found = smol::unblock(move || census(roots)).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                        app.apply_census(found, cx);
                        then(app, cx);
                    })
                });
            },
        )
        .detach();
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
        let found = smol::unblock(move || census(roots)).await;
        cx.update(|cx| {
            this.update(cx, |app: &mut Self, cx: &mut gpui::Context<Self>| {
                app.apply_census(found, cx);
            })
        })
        .is_ok()
    }
}
