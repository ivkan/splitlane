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
//! Nothing here is current. The window decided every word, and nobody has
//! decided anything since it closed: an agent that was working may have
//! finished long ago. Every answer says so (`window: "closed"`, and how long
//! ago the window last spoke), and the human output prints it.

use serde_json::{Value, json};
use splitlane_host::{Client, HostError, SessionInfo};
use splitlane_ipc_client::IpcTransport;

/// What the CLI answers from while the window is closed: the running
/// sessions the host holds and what a window last said about each.
pub struct ClosedWindow {
    sessions: Vec<SessionInfo>,
    now_ms: u64,
    /// For `read`, which asks the host for each session's text.
    client: Option<Client>,
}

/// What is said for a request the closed window cannot answer.
pub(super) const ONLY_READS: &str = "Splitlane is not running. With its window closed, only `ps`, \
     `status`, `answer` and `read` work; start Splitlane for the rest.";

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
            sessions: sessions
                .into_iter()
                .filter(|session| session.exited.is_none())
                .collect(),
            now_ms,
            client: None,
        }
    }

    /// Each session a window published a state for, with that state.
    fn published(&self) -> impl Iterator<Item = (&SessionInfo, &Value, u64)> {
        self.sessions.iter().filter_map(|session| {
            let published = session.agent_state.as_ref()?;
            published.state.is_object().then_some((
                session,
                &published.state,
                published.published_at_ms,
            ))
        })
    }

    fn surface_id(state: &Value) -> Option<u64> {
        state.get("surface_id").and_then(Value::as_u64)
    }

    fn seen_ago_secs(&self, published_at_ms: u64) -> u64 {
        self.now_ms.saturating_sub(published_at_ms) / 1000
    }

    fn surfaces(&self) -> Value {
        let surfaces: Vec<Value> = self
            .published()
            .filter_map(|(session, state, _)| {
                Some(json!({
                    "surface_id": Self::surface_id(state)?,
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
                    "state": state_word(state),
                    "hooked": state.get("source").and_then(Value::as_str) == Some("hook"),
                    "reason": Value::Null,
                    "surface_id": Self::surface_id(state),
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
            "state": state_word(state),
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
            _ => Err(ONLY_READS.to_string()),
        }
    }
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
                "surface.send_text",
                json!({ "surface_id": 7, "text": "hi" }),
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
    fn an_exited_session_is_not_listed() {
        let mut ended = session(Some(rail(ThreadStatus::Idle)), 0);
        ended.exited = Some(splitlane_host::ExitStatus {
            code: Some(0),
            signal: None,
        });
        let closed = ClosedWindow::from_sessions(vec![ended], 0);
        let fleet = closed.call("fleet.list", json!({})).unwrap();
        assert!(fleet["agents"].as_array().unwrap().is_empty());
    }
}
