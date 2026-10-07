//! Sessions opened by other sessions.
//!
//! An agent in a pane can open agent sessions of its own and work through
//! them. What it may do is decided here, on the server, from two facts the
//! caller cannot choose: which pane its process is running under
//! ([`crate::caller`]) and which session opened which
//! ([`crate::project::Thread::opened_by`]). The reasoning behind each rule is
//! in `docs/internals/design-decisions.md`, "A session works through the
//! sessions it opened".

use std::collections::HashMap;
use std::time::Duration;

use gpui::{App, Context, Entity};

use crate::SplitlaneApp;
use crate::app::ipc_handler::{JSONRPC_ERROR_KEY, JsonRpcError};
use crate::app::targeting::PaneRefusal;
use crate::caller::CallerLineage;
use crate::pane::TabContent;
use crate::terminal::view::TerminalView;

/// How many sessions one session may have open at a time.
///
/// Counted in sessions and not in panes: a session with no pane to go to is
/// opened without one, so room on screen is never what stops the ninth.
pub(crate) const MAX_OPENED_SESSIONS: usize = 8;

/// The text put in front of whatever one session writes into another.
///
/// It lands in the receiving agent's own transcript, so the agent, a person
/// reading the pane and anybody opening the conversation later all see that
/// no person typed what follows.
pub(crate) fn provenance_line(sender_title: &str) -> String {
    format!(
        "[Splitlane] Sent by the agent session {}, not typed by a person.",
        crate::app::send_answer::quoted_title(sender_title)
    )
}

/// How long an opening prompt waits for the new session's agent to be spoken
/// for, by its own file or by a hook frame, before it is given up on.
const OPENING_PROMPT_AGENT_MAX: Duration = Duration::from_secs(30);
const OPENING_PROMPT_POLL: Duration = Duration::from_millis(200);
/// And then for its screen to stop changing.
const OPENING_PROMPT_SETTLE_FLOOR: Duration = Duration::from_millis(700);
const OPENING_PROMPT_SETTLE_MAX: Duration = Duration::from_secs(8);

/// Why a request was refused. Nothing was done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The caller is in no pane of this app, and the app was not launched
    /// with the variable that lets a script outside one do this.
    NotFromAPane,
    /// The caller already has [`MAX_OPENED_SESSIONS`] sessions running.
    WorkerCeiling,
    /// The caller was itself opened by a session.
    OpenedSessionCannotOpen,
    /// A pane was required and there is none to give.
    NoRoom,
}

impl Refusal {
    /// The JSON-RPC code every refusal travels under.
    pub(crate) const CODE: i32 = -32004;

    /// The word a program branches on. Published: do not reword.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Refusal::NotFromAPane => "not_from_a_pane",
            Refusal::WorkerCeiling => "worker_ceiling",
            Refusal::OpenedSessionCannotOpen => "opened_session_cannot_open",
            Refusal::NoRoom => "no_room",
        }
    }

    fn sentence(self) -> String {
        match self {
            Refusal::NotFromAPane => "the caller is not running in a pane of this app; a script \
                 outside one needs SPLITLANE_IPC_ORCHESTRATION=1 (SPLITLANE_IPC_SCRIPTING=1 to \
                 submit) set where Splitlane was launched"
                .to_string(),
            Refusal::WorkerCeiling => format!(
                "this session already has {MAX_OPENED_SESSIONS} sessions it opened still \
                 running; close one first"
            ),
            Refusal::OpenedSessionCannotOpen => "a session opened by another session cannot \
                 open sessions; do the task in this one"
                .to_string(),
            Refusal::NoRoom => "no pane is empty and another would not fit; ask without \
                 requiring a pane to open the session in the rail"
                .to_string(),
        }
    }

    pub(crate) fn into_value(self, method: &str) -> serde_json::Value {
        serde_json::json!({
            JSONRPC_ERROR_KEY: {
                "code": Self::CODE,
                "message": format!("{method} refused ({}): {}", self.word(), self.sentence()),
                "data": { "reason": self.word() },
            }
        })
    }
}

/// The pane a call came from, with what the rules need to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneCaller {
    pub surface_id: u64,
    pub thread_id: u64,
    pub ws_idx: usize,
    pub title: String,
    /// Whether this session was itself opened by a session.
    pub opened: bool,
}

/// Who is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A process in no pane of this app: a person's script, or anything the
    /// process table could not place.
    Outside,
    Pane(PaneCaller),
}

/// The variables the app was launched with. They are what a caller outside a
/// pane is allowed by, and a pane cannot change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaunchGates {
    pub orchestration: bool,
    pub scripting: bool,
}

impl LaunchGates {
    pub(crate) fn read() -> Self {
        Self {
            orchestration: crate::app::ipc_handler::ipc_orchestration_enabled(),
            scripting: crate::app::ipc_handler::ipc_scripting_enabled(),
        }
    }
}

/// Whether `caller` may open a session, having `already_open` running.
///
/// A caller in a pane needs nothing switched on. It is bounded instead: by a
/// count, and by depth - a session that was opened by one opens none.
pub(crate) fn may_open(
    caller: &Caller,
    already_open: usize,
    gates: LaunchGates,
    submit: bool,
) -> Result<(), Refusal> {
    match caller {
        Caller::Pane(pane) => {
            if pane.opened {
                return Err(Refusal::OpenedSessionCannotOpen);
            }
            if already_open >= MAX_OPENED_SESSIONS {
                return Err(Refusal::WorkerCeiling);
            }
            Ok(())
        }
        Caller::Outside => {
            let open = if submit {
                gates.scripting
            } else {
                gates.orchestration
            };
            if open {
                Ok(())
            } else {
                Err(Refusal::NotFromAPane)
            }
        }
    }
}

/// What the caller asked for about a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlacementAsk {
    /// A pane if one is free or fits, the rail otherwise.
    Auto,
    /// The rail, whatever the screen looks like.
    Parked,
    /// A pane, or a refusal.
    Pane,
}

impl PlacementAsk {
    fn parse(value: Option<&serde_json::Value>) -> Result<Self, JsonRpcError> {
        match value.map(|v| v.as_str()) {
            None | Some(Some("auto")) => Ok(Self::Auto),
            Some(Some("parked")) => Ok(Self::Parked),
            Some(Some("pane")) => Ok(Self::Pane),
            Some(_) => Err(JsonRpcError::invalid_params(
                "placement must be \"auto\", \"parked\" or \"pane\"",
            )),
        }
    }
}

/// Why a session was opened without a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParkedBecause {
    Requested,
    Ceiling,
    TooNarrow,
}

impl ParkedBecause {
    fn word(self) -> &'static str {
        match self {
            ParkedBecause::Requested => "requested",
            ParkedBecause::Ceiling => "ceiling",
            ParkedBecause::TooNarrow => "too_narrow",
        }
    }
}

/// Where a newly opened session goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A pane that is showing nothing.
    EmptyPane,
    /// A pane added for it.
    NewPane,
    /// No pane: a row in the rail and a running process.
    Parked(ParkedBecause),
}

/// Choose where a newly opened session goes.
///
/// There is no rung that replaces what a pane is showing. The ordinary ladder
/// ends on "the focused pane, replaced", and the focused pane is as a rule the
/// caller's own - an agent opening five sessions would push itself and four
/// of them off the screen. A session with nowhere to go is opened in the rail
/// instead, where it has a row, a status and a place in the attention queue.
///
/// `another_pane` is the answer to "can a pane be added", `None` meaning yes.
pub(crate) fn place_opened(
    ask: PlacementAsk,
    has_empty_pane: bool,
    another_pane: Option<PaneRefusal>,
) -> Result<Placement, Refusal> {
    if ask == PlacementAsk::Parked {
        return Ok(Placement::Parked(ParkedBecause::Requested));
    }
    if has_empty_pane {
        return Ok(Placement::EmptyPane);
    }
    let because = match another_pane {
        // A project showing no pane at all takes the session as its first.
        None | Some(PaneRefusal::NoPanes) => return Ok(Placement::NewPane),
        Some(PaneRefusal::EmptyPaneAlready) => return Ok(Placement::EmptyPane),
        Some(PaneRefusal::Ceiling) => ParkedBecause::Ceiling,
        Some(PaneRefusal::TooNarrow { .. } | PaneRefusal::TooNarrowGridFits { .. }) => {
            ParkedBecause::TooNarrow
        }
    };
    match ask {
        PlacementAsk::Pane => Err(Refusal::NoRoom),
        PlacementAsk::Auto | PlacementAsk::Parked => Ok(Placement::Parked(because)),
    }
}

impl SplitlaneApp {
    /// Every terminal this app holds, on screen or not, with the id of its
    /// surface record when it has one.
    fn held_terminals(&self, cx: &App) -> Vec<(Entity<TerminalView>, Option<u64>)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for container in &self.workspaces {
            for pane in container.collect_panes() {
                for terminal in pane.read(cx).terminals() {
                    if seen.insert(terminal.entity_id()) {
                        out.push((terminal.clone(), terminal.read(cx).agent_thread_id));
                    }
                }
            }
        }
        for (thread_id, terminal) in &self.agents_view.agents_terminal_view_cache {
            if seen.insert(terminal.entity_id()) {
                out.push((terminal.clone(), Some(*thread_id)));
            }
        }
        out
    }

    /// Place a call in the pane it came from.
    pub(crate) fn caller_of(&self, lineage: &CallerLineage, cx: &App) -> Caller {
        let mut pty_children: HashMap<u32, (u64, Option<u64>)> = HashMap::new();
        for (terminal, thread_id) in self.held_terminals(cx) {
            let view = terminal.read(cx);
            // A pane whose shell has exited no longer vouches for anything:
            // its pid is free to be handed to an unrelated process.
            if view.terminal.child_pid == 0 || view.terminal.exited.is_some() {
                continue;
            }
            pty_children.insert(
                view.terminal.child_pid,
                (terminal.entity_id().as_u64(), thread_id),
            );
        }
        let Some((surface_id, Some(thread_id))) = crate::caller::pane_of(lineage, &pty_children)
        else {
            return Caller::Outside;
        };
        let found = self
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, container)| {
                let thread = container.threads.iter().find(|t| t.id == thread_id)?;
                Some(PaneCaller {
                    surface_id,
                    thread_id,
                    ws_idx,
                    title: thread.title.clone(),
                    opened: thread.opened_by.is_some(),
                })
            });
        found.map_or(Caller::Outside, Caller::Pane)
    }

    /// How many of the sessions `opener` opened still have a process.
    ///
    /// A session whose agent has exited, and one restored from a file that
    /// nobody has started yet, hold no place: the limit is on what is running.
    fn running_sessions_opened_by(&self, opener: u64, cx: &App) -> usize {
        let running: std::collections::HashSet<u64> = self
            .held_terminals(cx)
            .into_iter()
            .filter(|(terminal, _)| terminal.read(cx).terminal.exited.is_none())
            .filter_map(|(_, thread_id)| thread_id)
            .collect();
        self.workspaces
            .iter()
            .flat_map(|container| container.threads.iter())
            .filter(|thread| {
                thread.opened_by.as_ref().is_some_and(|by| by.id == opener)
                    && !thread.rail.agent_exited
                    && running.contains(&thread.id)
            })
            .count()
    }

    /// `surface.add_agent`: open an agent session the way the launcher does,
    /// for a caller that is another session or a person's script.
    pub(crate) fn handle_add_agent(
        &mut self,
        params: &serde_json::Value,
        lineage: &CallerLineage,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        const METHOD: &str = "surface.add_agent";
        let agent_name = params.get("agent").and_then(|a| a.as_str()).unwrap_or("");
        let Some(agent) = crate::agent_launcher::TerminalAgent::from_tag(agent_name)
            .or_else(|| crate::agent_launcher::TerminalAgent::from_binary(agent_name))
        else {
            let known: Vec<&str> = crate::agent_launcher::TerminalAgent::ALL
                .iter()
                .map(|a| a.tag())
                .collect();
            return JsonRpcError::invalid_params(format!(
                "Missing or unknown 'agent'; one of: {}",
                known.join(", ")
            ))
            .into_value();
        };
        let ask = match PlacementAsk::parse(params.get("placement")) {
            Ok(ask) => ask,
            Err(e) => return e.into_value(),
        };
        let name = match params.get("name") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => match value
                .as_str()
                .and_then(crate::app::ipc_handler::sanitize_pane_name)
            {
                Some(name) => Some(name),
                None => {
                    return JsonRpcError::invalid_params("'name' is empty or not a usable name")
                        .into_value();
                }
            },
        };
        let prompt = params
            .get("prompt")
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty());
        const MAX_PROMPT_LEN: usize = 64 * 1024;
        if prompt.is_some_and(|p| p.len() > MAX_PROMPT_LEN) {
            return JsonRpcError::invalid_params("Prompt exceeds 64 KiB limit").into_value();
        }
        let submit = params
            .get("submit")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        if submit && prompt.is_none() {
            return JsonRpcError::invalid_params("'submit' needs a 'prompt' to submit")
                .into_value();
        }

        let caller = self.caller_of(lineage, cx);
        let already_open = match &caller {
            Caller::Pane(pane) => self.running_sessions_opened_by(pane.thread_id, cx),
            Caller::Outside => 0,
        };
        if let Err(refusal) = may_open(&caller, already_open, LaunchGates::read(), submit) {
            return refusal.into_value(METHOD);
        }
        if !agent.is_installed() {
            return JsonRpcError::invalid_params(format!(
                "{} is not installed",
                agent.display_name()
            ))
            .into_value();
        }
        // The caller's own project. A script outside a pane has none, and
        // gets the one on screen, like every other call without a target.
        let ws_idx = match &caller {
            Caller::Pane(pane) => pane.ws_idx,
            Caller::Outside => self.active_idx,
        };
        let Some(container) = self.workspaces.get(ws_idx) else {
            return JsonRpcError::invalid_params("No project to open the session in").into_value();
        };
        let empty_pane = container.root.as_ref().and_then(|root| {
            root.collect_leaves()
                .into_iter()
                .find(|pane| pane.read(cx).tabs.is_empty())
        });
        // Decided before anything is created, so a refusal leaves nothing.
        let placement = match place_opened(
            ask,
            empty_pane.is_some(),
            self.refuse_another_pane_in(ws_idx, cx),
        ) {
            Ok(placement) => placement,
            Err(refusal) => return refusal.into_value(METHOD),
        };

        let title = name
            .clone()
            .unwrap_or_else(|| agent.display_name().to_string());
        let thread_id = match self.add_terminal_thread(ws_idx, title, Some(agent), cx) {
            Ok(id) => id,
            Err(err) => {
                return JsonRpcError::invalid_params(format!("Could not open a session: {err:?}"))
                    .into_value();
            }
        };
        let opened_by = match &caller {
            Caller::Pane(pane) => Some(splitlane_config::schema::OpenedBy {
                id: pane.thread_id,
                title: pane.title.clone(),
            }),
            Caller::Outside => None,
        };
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.opened_by = opened_by;
            // A name the opener chose is a name somebody chose: the agent's
            // own title must not replace it, or the opener could no longer
            // find the session by the name it gave.
            thread.title_user_set = name.is_some();
        }
        let Some(thread_idx) = self.thread_index_by_id(ws_idx, thread_id) else {
            return JsonRpcError::invalid_params("The session vanished while opening").into_value();
        };
        let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
        let Some(view) = self.mount_agents_terminal_for_target(target, cx) else {
            // An error must not leave a row behind that the caller was told
            // does not exist.
            let _ = self.remove_thread(ws_idx, thread_idx, cx);
            return JsonRpcError::invalid_params("The session could not be started").into_value();
        };
        // Neither arm moves the keyboard or changes which project is on
        // screen: the person is typing somewhere, and a session opening is
        // not a reason to take that away.
        let placement = match placement {
            Placement::EmptyPane => match empty_pane {
                Some(pane) => {
                    pane.update(cx, |pane, cx| pane.show_terminal(view.clone(), cx));
                    placement
                }
                None => Placement::Parked(ParkedBecause::Ceiling),
            },
            Placement::NewPane => {
                match self.append_pane_with(ws_idx, TabContent::Terminal(view.clone()), cx) {
                    Some(_) => placement,
                    // The tree did not take another leaf after all.
                    None => Placement::Parked(ParkedBecause::Ceiling),
                }
            }
            Placement::Parked(_) => placement,
        };
        // A pane starts its process when it is first drawn. A session in the
        // rail is never drawn, and one in a project that is not on screen is
        // not drawn yet, so it is started here.
        view.update(cx, |view, cx| view.ensure_backend_started(cx));

        if let Some(prompt) = prompt {
            let text = match &caller {
                Caller::Pane(pane) => format!("{}\n\n{prompt}", provenance_line(&pane.title)),
                Caller::Outside => prompt.to_string(),
            };
            self.schedule_opening_prompt(thread_id, &view, text, submit, cx);
        }
        self.save_session(cx);
        cx.notify();

        let rail = self.rail_snapshot_for(&view, Some(thread_id), cx);
        let (placement_word, placement_reason) = match placement {
            Placement::EmptyPane | Placement::NewPane => ("pane", None),
            Placement::Parked(because) => ("parked", Some(because.word())),
        };
        serde_json::json!({
            "surface_id": view.entity_id().as_u64(),
            "thread_id": thread_id,
            "agent": agent.binary(),
            "tier": rail.as_ref().map(|rail| rail.source.tier()),
            "session_id": self.thread_by_id(thread_id).and_then(|t| t.session_id.clone()),
            // The line a wait is measured from: a run past this one has ended.
            "runs_ended": rail.as_ref().map_or(0, |rail| rail.runs_ended),
            "placement": placement_word,
            "placement_reason": placement_reason,
            "opened_by": match &caller {
                Caller::Pane(pane) => Some(pane.surface_id),
                Caller::Outside => None,
            },
        })
    }

    /// Write a new session's first prompt once its agent is there to read it.
    ///
    /// The launch command is typed into the pane's shell, so for the first
    /// moments the thing reading input is the shell and not the agent. The
    /// prompt waits until something speaks for the agent - its own file read
    /// by the detector, or a hook frame - and then for the screen to settle.
    /// If nothing ever does, the prompt is dropped: an agent that did not
    /// start leaves a shell at the other end, and text sent to a shell is a
    /// command.
    /// It goes through the same write as `surface.send_text`: a paste, and
    /// when it is to be submitted, a carriage return of its own afterwards.
    fn schedule_opening_prompt(
        &mut self,
        thread_id: u64,
        terminal: &Entity<TerminalView>,
        text: String,
        submit: bool,
        cx: &mut Context<Self>,
    ) {
        let weak = terminal.downgrade();
        let submit_floor =
            Duration::from_millis(self.cached_config.resolved_submit_paste_delay_ms());
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut waited = Duration::ZERO;
                loop {
                    let spoken_for = cx.update(|cx| {
                        this.update(cx, |app, _| {
                            app.thread_by_id(thread_id)
                                .map(|t| t.detector_read_at.is_some() || t.hook_has_spoken)
                        })
                    });
                    match spoken_for {
                        Ok(Some(true)) => break,
                        Ok(Some(false)) => {}
                        // The session was closed, or the app is going away.
                        Ok(None) | Err(_) => return,
                    }
                    if waited >= OPENING_PROMPT_AGENT_MAX {
                        // Not written. With no agent there, the thing reading
                        // the pane's input is a shell, and the prompt would be
                        // run as a command.
                        log::warn!(
                            "opening prompt: nothing spoke for the agent of surface record \
                             {thread_id} within {OPENING_PROMPT_AGENT_MAX:?}; the prompt was \
                             not written"
                        );
                        return;
                    }
                    smol::Timer::after(OPENING_PROMPT_POLL).await;
                    waited += OPENING_PROMPT_POLL;
                }
                if Self::wait_for_terminal_settle(
                    &weak,
                    OPENING_PROMPT_SETTLE_FLOOR,
                    OPENING_PROMPT_SETTLE_MAX,
                    OPENING_PROMPT_POLL,
                    cx,
                )
                .await
                .is_none()
                {
                    return;
                }
                let written = cx.update(|cx| {
                    let Some(terminal) = weak.upgrade() else {
                        return false;
                    };
                    let view = terminal.read(cx);
                    // Outside a paste every newline is an Enter, and the text
                    // would be submitted in pieces. Not writing it is the
                    // smaller harm: the session is there and can be sent to.
                    if (text.contains('\n') || text.contains('\r'))
                        && !view.bracketed_paste_enabled()
                    {
                        return false;
                    }
                    view.inject_text(&text);
                    true
                });
                if !written {
                    log::warn!(
                        "opening prompt: surface record {thread_id} does not take a paste; \
                         the prompt was not written"
                    );
                    return;
                }
                if submit {
                    let _ = cx.update(|cx| {
                        this.update(cx, |_, cx| {
                            if let Some(terminal) = weak.upgrade() {
                                Self::schedule_deferred_submit(&terminal, submit_floor, cx);
                            }
                        })
                    });
                }
            },
        )
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(opened: bool) -> Caller {
        Caller::Pane(PaneCaller {
            surface_id: 7,
            thread_id: 3,
            ws_idx: 0,
            title: "lead".to_string(),
            opened,
        })
    }

    const NO_GATES: LaunchGates = LaunchGates {
        orchestration: false,
        scripting: false,
    };

    /// The point of the whole arrangement: nothing has to be switched on.
    #[test]
    fn a_session_in_a_pane_opens_sessions_with_nothing_switched_on() {
        assert_eq!(may_open(&pane(false), 0, NO_GATES, false), Ok(()));
        assert_eq!(may_open(&pane(false), 0, NO_GATES, true), Ok(()));
        assert_eq!(
            may_open(&pane(false), MAX_OPENED_SESSIONS - 1, NO_GATES, true),
            Ok(())
        );
    }

    #[test]
    fn the_ninth_session_is_refused() {
        assert_eq!(
            may_open(&pane(false), MAX_OPENED_SESSIONS, NO_GATES, false),
            Err(Refusal::WorkerCeiling)
        );
    }

    /// Depth one. The variables do not lift it: they are for a caller outside
    /// a pane, and this one is in one.
    #[test]
    fn a_session_that_was_opened_opens_none() {
        let all = LaunchGates {
            orchestration: true,
            scripting: true,
        };
        assert_eq!(
            may_open(&pane(true), 0, NO_GATES, false),
            Err(Refusal::OpenedSessionCannotOpen)
        );
        assert_eq!(
            may_open(&pane(true), 0, all, false),
            Err(Refusal::OpenedSessionCannotOpen)
        );
    }

    #[test]
    fn a_caller_outside_a_pane_is_let_in_by_the_launch_variables_only() {
        let orchestration = LaunchGates {
            orchestration: true,
            scripting: false,
        };
        let scripting = LaunchGates {
            orchestration: true,
            scripting: true,
        };
        assert_eq!(
            may_open(&Caller::Outside, 0, NO_GATES, false),
            Err(Refusal::NotFromAPane)
        );
        assert_eq!(may_open(&Caller::Outside, 0, orchestration, false), Ok(()));
        // Submitting is typing Enter into a pane, which the narrower variable
        // has never opened.
        assert_eq!(
            may_open(&Caller::Outside, 0, orchestration, true),
            Err(Refusal::NotFromAPane)
        );
        assert_eq!(may_open(&Caller::Outside, 0, scripting, true), Ok(()));
    }

    #[test]
    fn an_empty_pane_is_taken_before_one_is_added() {
        assert_eq!(
            place_opened(PlacementAsk::Auto, true, None),
            Ok(Placement::EmptyPane)
        );
        assert_eq!(
            place_opened(PlacementAsk::Auto, true, Some(PaneRefusal::Ceiling)),
            Ok(Placement::EmptyPane)
        );
        assert_eq!(
            place_opened(PlacementAsk::Pane, true, Some(PaneRefusal::Ceiling)),
            Ok(Placement::EmptyPane)
        );
    }

    #[test]
    fn a_pane_is_added_where_one_fits() {
        assert_eq!(
            place_opened(PlacementAsk::Auto, false, None),
            Ok(Placement::NewPane)
        );
        assert_eq!(
            place_opened(PlacementAsk::Auto, false, Some(PaneRefusal::NoPanes)),
            Ok(Placement::NewPane)
        );
    }

    /// No room is not a refusal, and it is never a replacement: there is no
    /// answer here that puts the session where another one is showing.
    #[test]
    fn with_no_room_the_session_opens_in_the_rail_and_says_why() {
        assert_eq!(
            place_opened(PlacementAsk::Auto, false, Some(PaneRefusal::Ceiling)),
            Ok(Placement::Parked(ParkedBecause::Ceiling))
        );
        assert_eq!(
            place_opened(
                PlacementAsk::Auto,
                false,
                Some(PaneRefusal::TooNarrow { would_be: 3 })
            ),
            Ok(Placement::Parked(ParkedBecause::TooNarrow))
        );
        assert_eq!(
            place_opened(
                PlacementAsk::Auto,
                false,
                Some(PaneRefusal::TooNarrowGridFits {
                    would_be: 3,
                    stacked: false
                })
            ),
            Ok(Placement::Parked(ParkedBecause::TooNarrow))
        );
    }

    #[test]
    fn a_required_pane_that_cannot_be_given_is_refused() {
        assert_eq!(
            place_opened(PlacementAsk::Pane, false, Some(PaneRefusal::Ceiling)),
            Err(Refusal::NoRoom)
        );
        assert_eq!(
            place_opened(
                PlacementAsk::Pane,
                false,
                Some(PaneRefusal::TooNarrow { would_be: 2 })
            ),
            Err(Refusal::NoRoom)
        );
    }

    #[test]
    fn asking_for_the_rail_leaves_the_layout_alone() {
        assert_eq!(
            place_opened(PlacementAsk::Parked, true, None),
            Ok(Placement::Parked(ParkedBecause::Requested))
        );
    }

    #[test]
    fn placement_is_one_of_three_words() {
        use serde_json::json;
        assert_eq!(PlacementAsk::parse(None), Ok(PlacementAsk::Auto));
        assert_eq!(
            PlacementAsk::parse(Some(&json!("parked"))),
            Ok(PlacementAsk::Parked)
        );
        assert_eq!(
            PlacementAsk::parse(Some(&json!("pane"))),
            Ok(PlacementAsk::Pane)
        );
        assert!(PlacementAsk::parse(Some(&json!("left"))).is_err());
        assert!(PlacementAsk::parse(Some(&json!(1))).is_err());
    }

    #[test]
    fn a_refusal_carries_its_word_where_a_program_reads_it() {
        let value = Refusal::WorkerCeiling.into_value("surface.add_agent");
        let error = &value[JSONRPC_ERROR_KEY];
        assert_eq!(error["code"], Refusal::CODE);
        assert_eq!(error["data"]["reason"], "worker_ceiling");
        let promoted = crate::app::ipc_handler::promote_response(value, serde_json::json!(1));
        assert_eq!(promoted["error"]["code"], -32004);
        assert_eq!(promoted["error"]["data"]["reason"], "worker_ceiling");
    }

    /// The sender's name is somebody else's text on an agent's input line.
    #[test]
    fn the_provenance_line_cannot_be_broken_by_a_session_name() {
        assert_eq!(
            provenance_line("lead"),
            "[Splitlane] Sent by the agent session \"lead\", not typed by a person."
        );
        let hostile = provenance_line("a\"b\n\x1b[201~c");
        assert!(!hostile.contains('\n'));
        assert!(!hostile.contains('\x1b'));
        assert_eq!(hostile.matches('"').count(), 2);
    }
}
