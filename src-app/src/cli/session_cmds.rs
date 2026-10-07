//! The verbs a session uses on the sessions it opened: `add`, `park`, `show`,
//! `close`.
//!
//! Thin wrappers over the `surface.*` methods of the same names. What a caller
//! may do is decided by the server, which knows which pane the call came from;
//! a refusal comes back as its own exit code with the reason in words.

use serde_json::{Value, json};
use splitlane_ipc_client::IpcTransport;

use super::selector::resolve_target;
use super::{CliError, EXIT_OK, EXIT_REFUSED};

/// What `splitlane add` was asked for.
pub struct AddOptions {
    pub agent: String,
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub prompt_file: Option<std::path::PathBuf>,
    pub submit: bool,
    pub parked: bool,
    pub pane: bool,
    pub json: bool,
}

/// Turn a server error into the exit code it stands for: a refusal by a rule
/// is not a failure of the instance, and a script must be able to tell.
pub(super) fn call_error(message: String) -> CliError {
    if message.contains("splitlane error -32004:") {
        CliError {
            code: EXIT_REFUSED,
            message,
        }
    } else {
        CliError::runtime(message)
    }
}

/// `splitlane add --agent <tag> [--name N] [--prompt T | --prompt-file P]
/// [--submit] [--parked | --pane] [--json]`.
pub fn add(client: &impl IpcTransport, options: AddOptions) -> Result<i32, CliError> {
    let prompt = match (&options.prompt, &options.prompt_file) {
        (Some(prompt), _) => Some(prompt.clone()),
        (None, Some(path)) => Some(std::fs::read_to_string(path).map_err(|e| {
            CliError::runtime(format!("cannot read prompt file {}: {e}", path.display()))
        })?),
        (None, None) => None,
    };
    let mut params = json!({ "agent": options.agent });
    if let Some(name) = &options.name {
        params["name"] = json!(name);
    }
    if let Some(prompt) = prompt {
        params["prompt"] = json!(prompt);
    }
    if options.submit {
        params["submit"] = json!(true);
    }
    if options.parked {
        params["placement"] = json!("parked");
    } else if options.pane {
        params["placement"] = json!("pane");
    }
    let result = super::reject_legacy_error(
        client
            .call("surface.add_agent", params)
            .map_err(call_error)?,
    )?;
    if options.json {
        super::print_json(&result)?;
    } else {
        println!("{}", add_summary(&result));
    }
    Ok(EXIT_OK)
}

/// One line a person can read and a script can still cut: the surface id
/// first, then where the session went.
fn add_summary(result: &Value) -> String {
    let surface = result
        .get("surface_id")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let agent = result
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or("agent");
    let placement = result
        .get("placement")
        .and_then(Value::as_str)
        .unwrap_or("parked");
    let runs_ended = result
        .get("runs_ended")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let place = match (
        placement,
        result.get("placement_reason").and_then(Value::as_str),
    ) {
        ("parked", Some(reason)) => format!("in the rail, no pane ({reason})"),
        ("parked", None) => "in the rail, no pane".to_string(),
        _ => "in a pane".to_string(),
    };
    format!("{surface}\t{agent} opened {place}; runs_ended {runs_ended}")
}

/// `splitlane park <target> [--json]`: take a session out of its pane. It
/// keeps running and keeps its row in the rail.
pub fn park(client: &impl IpcTransport, target: &str, json: bool) -> Result<i32, CliError> {
    let surface_id = resolve_target(client, target)?;
    let result = super::reject_legacy_error(
        client
            .call("surface.park", json!({ "surface_id": surface_id }))
            .map_err(call_error)?,
    )?;
    if json {
        super::print_json(&result)?;
    } else {
        println!("{surface_id}\tin the rail, no pane");
    }
    Ok(EXIT_OK)
}

/// `splitlane show <target> [--beside <target>] [--direction h|v] [--json]`:
/// put a session that is in no pane into one.
pub fn show(
    client: &impl IpcTransport,
    target: &str,
    beside: Option<&str>,
    direction: Option<&str>,
    json: bool,
) -> Result<i32, CliError> {
    let surface_id = resolve_target(client, target)?;
    let mut params = json!({ "surface_id": surface_id });
    if let Some(beside) = beside {
        params["beside_surface_id"] = json!(resolve_target(client, beside)?);
    }
    if let Some(direction) = direction {
        params["direction"] = json!(direction);
    }
    let result =
        super::reject_legacy_error(client.call("surface.show", params).map_err(call_error)?)?;
    if json {
        super::print_json(&result)?;
    } else {
        match result.get("displaced_surface_id").and_then(Value::as_u64) {
            Some(displaced) => println!("{surface_id}\tin a pane, in place of {displaced}"),
            None => println!("{surface_id}\tin a pane"),
        }
    }
    Ok(EXIT_OK)
}

/// `splitlane close <target> [--stop-turn] [--json]`: stop a session and
/// drop its row. The conversation stays in the agent's own history.
pub fn close(
    client: &impl IpcTransport,
    target: &str,
    stop_turn: bool,
    json: bool,
) -> Result<i32, CliError> {
    let surface_id = resolve_target(client, target)?;
    let mut params = json!({ "surface_id": surface_id });
    if stop_turn {
        params["stop_turn"] = json!(true);
    }
    let result =
        super::reject_legacy_error(client.call("surface.close", params).map_err(call_error)?)?;
    if json {
        super::print_json(&result)?;
    } else {
        println!("{surface_id}\tclosed");
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Recording {
        calls: RefCell<Vec<(String, Value)>>,
        reply: Result<Value, String>,
    }
    impl Recording {
        fn new(reply: Result<Value, String>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                reply,
            }
        }
    }
    impl IpcTransport for Recording {
        fn call(&self, method: &str, params: Value) -> Result<Value, String> {
            self.calls.borrow_mut().push((method.to_string(), params));
            self.reply.clone()
        }
    }

    fn options(agent: &str) -> AddOptions {
        AddOptions {
            agent: agent.to_string(),
            name: None,
            prompt: None,
            prompt_file: None,
            submit: false,
            parked: false,
            pane: false,
            json: true,
        }
    }

    #[test]
    fn add_sends_only_what_was_asked_for() {
        let fake = Recording::new(Ok(json!({ "surface_id": 12, "placement": "pane" })));
        assert_eq!(add(&fake, options("claude_code")).expect("ok"), EXIT_OK);
        let calls = fake.calls.borrow();
        assert_eq!(calls[0].0, "surface.add_agent");
        assert_eq!(calls[0].1, json!({ "agent": "claude_code" }));
    }

    #[test]
    fn add_passes_the_name_the_prompt_and_where_to_put_it() {
        let fake = Recording::new(Ok(json!({ "surface_id": 12, "placement": "parked" })));
        let mut asked = options("codex");
        asked.name = Some("api-docs".to_string());
        asked.prompt = Some("write the docs".to_string());
        asked.submit = true;
        asked.parked = true;
        assert_eq!(add(&fake, asked).expect("ok"), EXIT_OK);
        let calls = fake.calls.borrow();
        assert_eq!(
            calls[0].1,
            json!({
                "agent": "codex",
                "name": "api-docs",
                "prompt": "write the docs",
                "submit": true,
                "placement": "parked",
            })
        );
    }

    #[test]
    fn a_refusal_has_its_own_exit_code() {
        let fake = Recording::new(Err(
            "splitlane error -32004: surface.add_agent refused (worker_ceiling): full".to_string(),
        ));
        let err = add(&fake, options("claude_code")).expect_err("refused");
        assert_eq!(err.code, EXIT_REFUSED);
        assert!(err.message.contains("worker_ceiling"));
    }

    #[test]
    fn any_other_error_is_a_runtime_failure() {
        let fake = Recording::new(Err("splitlane error -32602: unknown agent".to_string()));
        let err = add(&fake, options("nope")).expect_err("invalid");
        assert_eq!(err.code, crate::cli::EXIT_RUNTIME);
    }

    #[test]
    fn an_unreadable_prompt_file_stops_before_the_call() {
        let fake = Recording::new(Ok(json!({})));
        let mut asked = options("claude_code");
        asked.prompt_file = Some(std::path::PathBuf::from(
            "definitely-not-a-file-splitlane-add-test",
        ));
        let err = add(&fake, asked).expect_err("no file");
        assert_eq!(err.code, crate::cli::EXIT_RUNTIME);
        assert!(fake.calls.borrow().is_empty());
    }

    /// Answers `surface.list` with two surfaces and records everything else.
    struct Listed {
        calls: RefCell<Vec<(String, Value)>>,
        reply: Result<Value, String>,
    }
    impl IpcTransport for Listed {
        fn call(&self, method: &str, params: Value) -> Result<Value, String> {
            if method == "surface.list" {
                return Ok(json!({ "surfaces": [
                    { "surface_id": 12, "name": "api-docs" },
                    { "surface_id": 18, "name": "plan" },
                ]}));
            }
            self.calls.borrow_mut().push((method.to_string(), params));
            self.reply.clone()
        }
    }

    #[test]
    fn park_names_the_surface_it_resolved() {
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Ok(json!({ "parked": true, "surface_id": 12 })),
        };
        assert_eq!(park(&fake, "api-docs", true).expect("ok"), EXIT_OK);
        let calls = fake.calls.borrow();
        assert_eq!(calls[0].0, "surface.park");
        assert_eq!(calls[0].1, json!({ "surface_id": 12 }));
    }

    #[test]
    fn show_passes_the_neighbour_and_the_direction_only_when_given() {
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Ok(json!({ "shown": true, "surface_id": 12 })),
        };
        assert_eq!(
            show(&fake, "api-docs", None, None, true).expect("ok"),
            EXIT_OK
        );
        assert_eq!(
            show(&fake, "api-docs", Some("plan"), Some("horizontal"), true).expect("ok"),
            EXIT_OK
        );
        let calls = fake.calls.borrow();
        assert_eq!(calls[0].1, json!({ "surface_id": 12 }));
        assert_eq!(
            calls[1].1,
            json!({ "surface_id": 12, "beside_surface_id": 18, "direction": "horizontal" })
        );
    }

    #[test]
    fn a_refused_move_is_exit_8_and_an_unknown_target_is_exit_3() {
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Err("splitlane error -32004: surface.show refused (no_room): no".to_string()),
        };
        let err = show(&fake, "api-docs", None, None, false).expect_err("refused");
        assert_eq!(err.code, EXIT_REFUSED);
        let err = park(&fake, "nothing-by-this-name", false).expect_err("no target");
        assert_eq!(err.code, crate::cli::EXIT_TARGET);
    }

    #[test]
    fn close_stops_a_turn_only_when_told_to() {
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Ok(json!({ "closed": true, "surface_id": 12, "thread_id": 4 })),
        };
        assert_eq!(close(&fake, "api-docs", false, true).expect("ok"), EXIT_OK);
        assert_eq!(close(&fake, "api-docs", true, true).expect("ok"), EXIT_OK);
        let calls = fake.calls.borrow();
        assert_eq!(calls[0].0, "surface.close");
        assert_eq!(calls[0].1, json!({ "surface_id": 12 }));
        assert_eq!(calls[1].1, json!({ "surface_id": 12, "stop_turn": true }));
    }

    #[test]
    fn a_close_refused_for_a_turn_in_flight_is_exit_8() {
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Err(
                "splitlane error -32004: surface.close refused (turn_in_flight): no".to_string(),
            ),
        };
        let err = close(&fake, "api-docs", false, false).expect_err("refused");
        assert_eq!(err.code, EXIT_REFUSED);
        assert!(err.message.contains("turn_in_flight"));
    }

    #[test]
    fn the_summary_leads_with_the_surface_id() {
        let line = add_summary(&json!({
            "surface_id": 31, "agent": "claude", "placement": "parked",
            "placement_reason": "ceiling", "runs_ended": 0,
        }));
        assert_eq!(
            line,
            "31\tclaude opened in the rail, no pane (ceiling); runs_ended 0"
        );
        let line = add_summary(&json!({
            "surface_id": 32, "agent": "codex", "placement": "pane", "runs_ended": 0,
        }));
        assert_eq!(line, "32\tcodex opened in a pane; runs_ended 0");
    }
}
