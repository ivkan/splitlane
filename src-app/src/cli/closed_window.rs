//! The CLI's answers while no Splitlane window runs.
//!
//! A window hands the session host what it concluded about each agent whose
//! session the host holds (`splitlane_host::protocol::AgentState`). When the
//! window is gone those sessions go on, and so does the question of where
//! each agent stands. [`ClosedWindow`] answers the read-only methods `ps`,
//! `status`, `answer` and `read` use from what the host kept, in the shapes
//! the window would have used, so those commands run unchanged against it.
//! `read` is the one answer that is current: the host rebuilds the text
//! from the session's own output.
//!
//! `send` writes through the host, which decides whether it may: only a
//! host started by a Splitlane launched with `SPLITLANE_IPC_SCRIPTING=1`
//! takes text. The other way the window lets a write through, a session
//! writing into one it opened, needs the window's record of who opened what,
//! and is refused here.
//!
//! Nothing here is current. The window decided every word, and nobody has
//! decided anything since it closed: an agent that was working may have
//! finished long ago. Every answer says so (`window: "closed"`, and how long
//! ago the window last spoke), and the human output prints it.

use std::cell::{Cell, RefCell};

use serde_json::{Value, json};
use splitlane_host::protocol::ProgramReport;
use splitlane_host::{Client, HostError, SessionInfo};
use splitlane_ipc_client::IpcTransport;

/// What the CLI answers from while the window is closed: the running
/// sessions the host holds and what a window last said about each.
pub struct ClosedWindow {
    /// Read again on every request while there is a host to ask, so that
    /// `wait`, which asks over and over, sees the program's reports arrive.
    sessions: RefCell<Vec<SessionInfo>>,
    now_ms: Cell<u64>,
    /// For the requests that go to the host: `read`, `send`, and reading
    /// the sessions again.
    client: Option<Client>,
}

/// What is said for a request the closed window cannot answer.
pub(super) const ONLY_READS: &str = "Splitlane is not running. With its window closed, only `ps`, \
     `status`, `answer`, `read`, `send` and `wait --until turn-end` or `--state` work; start \
     Splitlane for the rest.";

/// What `send` is told when the host does not take writes.
const WRITES_OFF: &str = "send with the window closed goes through the session host, and the \
     Splitlane that started it was not launched with SPLITLANE_IPC_SCRIPTING=1. The host keeps \
     that setting while it holds sessions: start Splitlane and send through the window, or end \
     the sessions (`splitlane host stop`) and launch Splitlane with the variable set.";

impl ClosedWindow {
    /// The host's sessions, or `None` when no host runs: then there is
    /// nothing to answer from and the caller reports the window as missing.
    pub fn read() -> Option<Self> {
        let socket = crate::runtime_paths::host_socket_path_spec()?
            .path()
            .to_path_buf();
        let client = match Client::connect(&socket) {
            Ok(client) => client,
            Err(HostError::NotRunning | HostError::Unsupported) => return None,
            Err(e) => {
                log::debug!("session host unreachable: {e}");
                return None;
            }
        };
        let sessions = client.list().ok()?;
        let mut closed = Self::from_sessions(sessions, now_ms());
        closed.client = Some(client);
        Some(closed)
    }

    fn from_sessions(sessions: Vec<SessionInfo>, now_ms: u64) -> Self {
        Self {
            sessions: RefCell::new(sessions),
            now_ms: Cell::new(now_ms),
            client: None,
        }
    }

    /// Read the host's sessions again. A host that stopped answering
    /// leaves the last reading in place.
    ///
    /// A host that stops answering is an error: a `wait` polling a reading
    /// that can no longer change would sit out its whole timeout.
    fn refresh(&self) -> Result<(), String> {
        let Some(client) = self.client.as_ref() else {
            return Ok(());
        };
        let sessions = client
            .list()
            .map_err(|e| format!("the session host stopped answering: {e}"))?;
        *self.sessions.borrow_mut() = sessions;
        self.now_ms.set(now_ms());
        Ok(())
    }

    /// Each session a window published a state for, with that state as it
    /// reads now.
    ///
    /// Two things that happened after the window spoke change it:
    ///
    /// - The program reported on itself (OSC 7501). Its newest report is
    ///   taken over the window's older word, with the turns it saw end
    ///   added to the window's count, and marked as the program's own,
    ///   unchecked claim (`source: "program_report"`).
    /// - `send` wrote into the session and no report came after. The state
    ///   was decided before the write, and the program may be answering it
    ///   now, so it reads as a turn in flight: otherwise `answer` straight
    ///   after a `send` would hand back the answer before it, and say
    ///   nothing.
    fn published(&self) -> impl Iterator<Item = (SessionInfo, Value, u64)> {
        let sessions = self.sessions.borrow();
        let published: Vec<_> = sessions
            .iter()
            .filter_map(|session| {
                let published = session.agent_state.as_ref()?;
                if !published.state.is_object() {
                    return None;
                }
                let mut state = published.state.clone();
                // A session that ended stays, so that a `wait` on it hears
                // it ended rather than lose its target.
                if session.exited.is_some() {
                    state["exited"] = json!(true);
                }
                let sent = session
                    .sent_at_ms
                    .filter(|&sent| sent >= published.published_at_ms);
                let report = session.program_report.as_ref().filter(|report| {
                    report.at_ms > published.published_at_ms
                        && sent.is_none_or(|sent| report.at_ms > sent)
                });
                if let Some(report) = report {
                    apply_report(&mut state, report, published.turns_ended_at_publish);
                } else if sent.is_some() {
                    state["status"] = json!("running");
                    state["source"] = json!("none");
                    state["tier"] = json!("T3");
                    state["message"] = Value::Null;
                    state["sent_with_window_closed"] = json!(true);
                }
                Some((session.clone(), state, published.published_at_ms))
            })
            .collect();
        published.into_iter()
    }

    fn surface_id(state: &Value) -> Option<u64> {
        state.get("surface_id").and_then(Value::as_u64)
    }

    fn seen_ago_secs(&self, published_at_ms: u64) -> u64 {
        self.now_ms.get().saturating_sub(published_at_ms) / 1000
    }

    fn surfaces(&self) -> Value {
        let surfaces: Vec<Value> = self
            .published()
            .filter_map(|(session, state, _)| {
                Some(json!({
                    "surface_id": Self::surface_id(&state)?,
                    "name": state.get("surface_name"),
                    "cwd": session.cwd,
                    "cmd": state.get("agent"),
                }))
            })
            .collect();
        json!({ "surfaces": surfaces, "window": "closed" })
    }

    fn fleet(&self, all: bool) -> Value {
        let agents: Vec<Value> = self
            .published()
            .filter(|(_, state, _)| {
                state.get("agent").is_some_and(Value::is_string)
                    && state.get("exited").and_then(Value::as_bool) != Some(true)
            })
            .map(|(_, state, at)| {
                json!({
                    "pid": Value::Null,
                    "tool": state.get("agent"),
                    "state": state_word(&state),
                    "hooked": state.get("source").and_then(Value::as_str) == Some("hook"),
                    "reason": Value::Null,
                    "surface_id": Self::surface_id(&state),
                    "surface_name": state.get("surface_name"),
                    "workspace": Value::Null,
                    "project": state.get("workspace"),
                    "message": state.get("message"),
                    "rail": state,
                    "seen_secs_ago": self.seen_ago_secs(at),
                })
            })
            .collect();
        let mut result = json!({ "agents": agents, "window": "closed" });
        if all {
            // What runs in a shell is read from the process table by the
            // window; with no window nobody has looked.
            result["commands"] = json!([]);
            result["idle_shells"] = json!(0);
        }
        result
    }

    fn status(&self, surface_id: u64) -> Result<Value, String> {
        let (_, state, at) = self
            .published()
            .find(|(_, state, _)| Self::surface_id(state) == Some(surface_id))
            .ok_or_else(|| format!("no pane {surface_id} in what the window last saw"))?;
        Ok(json!({
            "surface_id": surface_id,
            "state": state_word(&state),
            "hooked": state.get("source").and_then(Value::as_str) == Some("hook"),
            "tool": state.get("agent"),
            "message": state.get("message"),
            "rail": state,
            "window": "closed",
            "seen_secs_ago": self.seen_ago_secs(at),
        }))
    }

    /// `surface.read`, as the window answers it, from the text the host
    /// rebuilds from the session's log.
    fn surface_text(&self, params: &Value) -> Result<Value, String> {
        use crate::app::ipc_handler::{
            paginate_scrollback, surface_read_value, truncate_ipc_text, wrap_untrusted,
        };
        // The window's defaults and bounds for the same request.
        const DEFAULT_LINES: usize = 200;
        let surface_id = params
            .get("surface_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "surface.read needs a surface_id".to_string())?;
        let (session, _, _) = self
            .published()
            .find(|(_, state, _)| Self::surface_id(state) == Some(surface_id))
            .ok_or_else(|| format!("no pane {surface_id} in what the window last saw"))?;
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| "the session host is not reachable".to_string())?;
        let read = client
            .read_text(session.id)
            .map_err(|e| match e.remote_code() {
                Some(splitlane_host::protocol::ERR_UNKNOWN_OP) => {
                    "the session host was started by an older Splitlane and cannot read a \
                     session's text; start Splitlane to read it"
                        .to_string()
                }
                _ => format!("session host: {e}"),
            })?;
        let lines = params
            .get("lines")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, splitlane_host::transcript::MAX_LINES))
            .unwrap_or(DEFAULT_LINES);
        let offset = params
            .get("offset")
            .and_then(Value::as_u64)
            .map_or(0, |n| n as usize);
        let full = read.text.unwrap_or_default();
        let (text, returned, total, eof) = paginate_scrollback(&full, lines, offset);
        if offset > total {
            return Err(format!(
                "offset {offset} out of range (total_lines={total})"
            ));
        }
        let fenced = params
            .get("fenced")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                splitlane_config::loader::load_config().ai_injection_fence_enabled()
            });
        let (text, truncated) = truncate_ipc_text(text);
        let text = if fenced {
            wrap_untrusted(
                &format!("source=\"surface:{surface_id}\" total_lines=\"{total}\" eof=\"{eof}\""),
                &text,
            )
        } else {
            text
        };
        // The log's end advances with every byte the session prints, which
        // is what the window's counter says too.
        let mut value =
            surface_read_value(text, returned, total, eof, session.log_tail_seq, truncated);
        value["window"] = json!("closed");
        value["gap"] = json!(read.gap);
        Ok(value)
    }

    /// `surface.send_text` for a caller the write setting lets through, as
    /// the host writes it. The reply leaves out what only the window knows,
    /// the agent behind the pane and whether a turn started, so the caller
    /// does not wait for a start nobody can confirm.
    fn send_text(&self, params: &Value) -> Result<Value, String> {
        let surface_id = params
            .get("surface_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "surface.send_text needs a surface_id".to_string())?;
        let (session, _, _) = self
            .published()
            .find(|(_, state, _)| Self::surface_id(state) == Some(surface_id))
            .ok_or_else(|| format!("no pane {surface_id} in what the window last saw"))?;
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| "the session host is not reachable".to_string())?;
        let text = params
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let submit = params.get("submit").and_then(Value::as_bool) == Some(true);
        let args = splitlane_host::protocol::SendTextArgs {
            session: session.id,
            text: text.clone(),
            submit,
            paste: params.get("paste").and_then(Value::as_bool),
            submit_delay_ms: Some(
                splitlane_config::loader::load_config().resolved_submit_paste_delay_ms(),
            ),
        };
        let sent = client.send_text(&args).map_err(|e| match e.remote_code() {
            Some(splitlane_host::protocol::ERR_FORBIDDEN) => WRITES_OFF.to_string(),
            Some(splitlane_host::protocol::ERR_UNKNOWN_OP) => {
                "the session host was started by an older Splitlane and cannot write into a \
                 session; start Splitlane to send"
                    .to_string()
            }
            _ => format!("session host: {e}"),
        })?;
        Ok(json!({
            "sent": true,
            "length": text.len(),
            "submitted": submit,
            "paste": sent.paste,
            "typed": false,
            "submit_mode": sent.submit_mode,
            "agent_target": false,
            "terminal_bracketed_paste": sent.bracketed_paste,
            "window": "closed",
        }))
    }

    /// How long ago the window last said anything, over every session.
    pub fn last_seen_secs_ago(&self) -> Option<u64> {
        self.published()
            .map(|(_, _, at)| at)
            .max()
            .map(|at| self.seen_ago_secs(at))
    }
}

impl IpcTransport for ClosedWindow {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.refresh()?;
        match method {
            "surface.list" => Ok(self.surfaces()),
            "fleet.list" => {
                Ok(self.fleet(params.get("all").and_then(Value::as_bool) == Some(true)))
            }
            "surface.status" => {
                let surface_id = params
                    .get("surface_id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "surface.status needs a surface_id".to_string())?;
                self.status(surface_id)
            }
            "surface.read" => self.surface_text(&params),
            "surface.send_text" => self.send_text(&params),
            _ => Err(ONLY_READS.to_string()),
        }
    }
}

/// Read the program's newest report into `state`, a window's rail object;
/// see [`ClosedWindow::published`]. `turns_at_publish` is the program's
/// count of ended turns when the window published `state`.
fn apply_report(state: &mut Value, report: &ProgramReport, turns_at_publish: u64) {
    let (status, message) = match report.state.as_deref() {
        Some("working") => ("running", None),
        Some("blocked") => (
            "waiting",
            report.msg.clone().or_else(|| report.kind.clone()),
        ),
        Some("error") => ("failed", None),
        // `done`, `idle`, and a record the program cleared.
        _ => ("idle", None),
    };
    let since = report.turns_ended.saturating_sub(turns_at_publish);
    if since > 0 {
        let runs = state.get("runs_ended").and_then(Value::as_u64).unwrap_or(0);
        state["runs_ended"] = json!(runs + since);
        state["last_outcome"] = json!(report.last_outcome);
    }
    state["status"] = json!(status);
    state["message"] = json!(message);
    state["source"] = json!("program_report");
    state["tier"] = json!("T3");
}

/// The older `state` word `fleet.list` and `surface.status` carry, from the
/// rail's status word; see `RailSnapshot::state_word`.
fn state_word(state: &Value) -> &'static str {
    match state.get("status").and_then(Value::as_str) {
        Some("running") => "thinking",
        Some("waiting") => "waiting_for_input",
        Some("failed") => "errored",
        _ => "idle",
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ThreadStatus;
    use crate::rail_state::{RailSnapshot, RailSource, status_word};
    use splitlane_host::protocol::AgentState;

    fn session(state: Option<Value>, published_at_ms: u64) -> SessionInfo {
        serde_json::from_value(json!({
            "id": splitlane_host::SessionId::new_random(),
            "cwd": "/work/demo",
            "agent_state": state.map(|state| AgentState {
                state,
                published_at_ms,
                live: false,
                turns_ended_at_publish: 0,
            }),
        }))
        .unwrap()
    }

    fn rail(status: ThreadStatus) -> Value {
        let mut state = RailSnapshot {
            status,
            source: RailSource::Hook,
            runs_ended: 2,
            last_outcome: None,
            turn_marker: None,
            background_shells: 0,
            exited: false,
            message: Some("Allow Bash?".into()),
            opening_prompt: None,
            agent: Some("claude"),
            session_id: Some("abc".into()),
            cwd: Some("/work/demo".into()),
        }
        .to_json();
        state["surface_id"] = json!(7);
        state["surface_name"] = json!("claude");
        state["workspace"] = json!("demo");
        state
    }

    #[test]
    fn the_state_word_follows_the_rail_word_for_every_status() {
        let pairs = [
            (ThreadStatus::Starting, "idle"),
            (ThreadStatus::Thinking, "thinking"),
            (ThreadStatus::WaitingForInput, "waiting_for_input"),
            (ThreadStatus::Idle, "idle"),
            (ThreadStatus::Failed, "errored"),
        ];
        for (status, word) in pairs {
            let snapshot = json!({ "status": status_word(status) });
            assert_eq!(state_word(&snapshot), word, "{status:?}");
        }
    }

    #[test]
    fn ps_lists_what_the_window_last_saw_and_says_how_long_ago() {
        let closed = ClosedWindow::from_sessions(
            vec![
                session(Some(rail(ThreadStatus::WaitingForInput)), 10_000),
                // A plain shell: the window published nothing for it.
                session(None, 0),
            ],
            70_000,
        );
        let fleet = closed.call("fleet.list", json!({})).unwrap();
        assert_eq!(fleet["window"], "closed");
        let agents = fleet["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["tool"], "claude");
        assert_eq!(agents[0]["state"], "waiting_for_input");
        assert_eq!(agents[0]["surface_name"], "claude");
        assert_eq!(agents[0]["project"], "demo");
        assert_eq!(agents[0]["seen_secs_ago"], 60);
        assert_eq!(closed.last_seen_secs_ago(), Some(60));
        let all = closed.call("fleet.list", json!({ "all": true })).unwrap();
        assert_eq!(all["commands"], json!([]));
    }

    #[test]
    fn status_and_targets_come_from_the_published_surface() {
        let closed =
            ClosedWindow::from_sessions(vec![session(Some(rail(ThreadStatus::Idle)), 0)], 0);
        let surfaces = closed.call("surface.list", json!({})).unwrap();
        assert_eq!(surfaces["surfaces"][0]["surface_id"], 7);
        assert_eq!(surfaces["surfaces"][0]["name"], "claude");
        let status = closed
            .call("surface.status", json!({ "surface_id": 7 }))
            .unwrap();
        assert_eq!(status["state"], "idle");
        // What `answer` needs to find the transcript by itself.
        assert_eq!(status["rail"]["agent"], "claude");
        assert_eq!(status["rail"]["session_id"], "abc");
        assert_eq!(status["rail"]["cwd"], "/work/demo");
        assert!(
            closed
                .call("surface.status", json!({ "surface_id": 8 }))
                .is_err()
        );
    }

    #[test]
    fn anything_that_needs_the_window_says_it_is_closed() {
        let closed = ClosedWindow::from_sessions(Vec::new(), 0);
        let error = closed
            .call(
                "surface.send_keystroke",
                json!({ "surface_id": 7, "keystroke": "esc" }),
            )
            .unwrap_err();
        assert!(error.contains("not running"), "{error}");
    }

    #[test]
    fn a_read_names_the_pane_and_needs_the_host() {
        let closed =
            ClosedWindow::from_sessions(vec![session(Some(rail(ThreadStatus::Idle)), 0)], 0);
        let unknown = closed
            .call("surface.read", json!({ "surface_id": 8 }))
            .unwrap_err();
        assert!(unknown.contains("no pane 8"), "{unknown}");
        let no_host = closed
            .call("surface.read", json!({ "surface_id": 7 }))
            .unwrap_err();
        assert!(no_host.contains("not reachable"), "{no_host}");
    }

    #[test]
    fn a_state_from_before_a_send_reads_as_a_turn_in_flight() {
        let mut written = session(Some(rail(ThreadStatus::Idle)), 1_000);
        written.sent_at_ms = Some(2_000);
        let closed = ClosedWindow::from_sessions(vec![written], 3_000);
        let status = closed
            .call("surface.status", json!({ "surface_id": 7 }))
            .unwrap();
        // `answer` refuses on this word rather than hand back the answer
        // the text was sent after.
        assert_eq!(status["rail"]["status"], "running");
        assert_eq!(status["rail"]["tier"], "T3");
        assert_eq!(status["rail"]["sent_with_window_closed"], true);
        assert_eq!(status["state"], "thinking");

        // A write the window saw and published after is not a turn in flight.
        let mut seen = session(Some(rail(ThreadStatus::Idle)), 3_000);
        seen.sent_at_ms = Some(2_000);
        let closed = ClosedWindow::from_sessions(vec![seen], 4_000);
        let status = closed
            .call("surface.status", json!({ "surface_id": 7 }))
            .unwrap();
        assert_eq!(status["rail"]["status"], "idle");
    }

    fn report(state: &str, turns_ended: u64, at_ms: u64) -> ProgramReport {
        ProgramReport {
            state: Some(state.into()),
            kind: (state == "blocked").then(|| "permission".to_string()),
            msg: (state == "blocked").then(|| "Allow Bash?".to_string()),
            at_ms,
            turns_ended,
            last_outcome: (turns_ended > 0).then(|| "finished".to_string()),
        }
    }

    fn status_of(session: SessionInfo) -> Value {
        ClosedWindow::from_sessions(vec![session], 10_000)
            .call("surface.status", json!({ "surface_id": 7 }))
            .unwrap()
    }

    #[test]
    fn a_turn_the_program_reports_ended_counts_after_the_windows() {
        // The window published at 1 000 with two runs ended and the
        // program's count at 3; the program then reported a fourth end.
        let mut ended = session(Some(rail(ThreadStatus::Thinking)), 1_000);
        ended.agent_state.as_mut().unwrap().turns_ended_at_publish = 3;
        ended.program_report = Some(report("done", 4, 2_000));
        let rail = &status_of(ended)["rail"];
        assert_eq!(rail["status"], "idle");
        assert_eq!(rail["runs_ended"], 3);
        assert_eq!(rail["last_outcome"], "finished");
        assert_eq!(rail["source"], "program_report");
        assert_eq!(rail["tier"], "T3");
    }

    #[test]
    fn a_blocked_report_reads_as_waiting_with_its_question() {
        let mut blocked = session(Some(rail(ThreadStatus::Thinking)), 1_000);
        blocked.program_report = Some(report("blocked", 0, 2_000));
        let status = status_of(blocked);
        assert_eq!(status["rail"]["status"], "waiting");
        assert_eq!(status["rail"]["message"], "Allow Bash?");
        assert_eq!(status["state"], "waiting_for_input");
    }

    #[test]
    fn a_report_older_than_the_window_or_the_send_does_not_count() {
        // Older than what the window published: the window's word stands.
        let mut old = session(Some(rail(ThreadStatus::Idle)), 3_000);
        old.program_report = Some(report("working", 0, 2_000));
        assert_eq!(status_of(old)["rail"]["status"], "idle");

        // Newer than the window, older than a send: the send stands.
        let mut sent = session(Some(rail(ThreadStatus::Idle)), 1_000);
        sent.program_report = Some(report("done", 1, 2_000));
        sent.sent_at_ms = Some(3_000);
        let read = &status_of(sent)["rail"];
        assert_eq!(read["status"], "running");
        assert_eq!(read["sent_with_window_closed"], true);

        // A report after the send: the program has answered it.
        let mut answered = session(Some(rail(ThreadStatus::Idle)), 1_000);
        answered.sent_at_ms = Some(2_000);
        answered.program_report = Some(report("done", 1, 3_000));
        let answer = &status_of(answered)["rail"];
        assert_eq!(answer["status"], "idle");
        assert_eq!(answer["runs_ended"], 3);
    }

    #[test]
    fn an_exited_session_is_not_listed_but_says_it_ended() {
        let mut ended = session(Some(rail(ThreadStatus::Idle)), 0);
        ended.exited = Some(splitlane_host::ExitStatus {
            code: Some(0),
            signal: None,
        });
        let closed = ClosedWindow::from_sessions(vec![ended], 0);
        let fleet = closed.call("fleet.list", json!({})).unwrap();
        assert!(fleet["agents"].as_array().unwrap().is_empty());
        // A `wait` on it is told it ended, which it reports as a failure,
        // rather than losing its target.
        let status = closed
            .call("surface.status", json!({ "surface_id": 7 }))
            .unwrap();
        assert_eq!(status["rail"]["exited"], true);
    }
}
