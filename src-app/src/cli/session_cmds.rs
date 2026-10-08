//! The verbs a session uses on the sessions it opened: `add`, `park`, `show`,
//! `close`, `interrupt`.
//!
//! Thin wrappers over the `surface.*` methods of the same names. What a caller
//! may do is decided by the server, which knows which pane the call came from;
//! a refusal comes back as its own exit code with the reason in words.

use serde_json::{Value, json};
use splitlane_ipc_client::IpcTransport;

use super::selector::resolve_target;
use super::{
    CliError, EXIT_ASKED_PERSON, EXIT_NO_TURN_SIGNAL, EXIT_OK, EXIT_REFUSED, EXIT_RUNTIME,
    EXIT_TIMEOUT,
};

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
    // A question put to a person is neither done nor refused.
    if message.contains("splitlane error -32005:") {
        return CliError {
            code: EXIT_ASKED_PERSON,
            message,
        };
    }
    if message.contains("splitlane error -32004:") {
        CliError {
            // Not a rule the caller ran into but a thing this build cannot
            // do for that agent: the code that says "use another method".
            code: if message.contains("refused (no_interrupt)") {
                EXIT_NO_TURN_SIGNAL
            } else {
                EXIT_REFUSED
            },
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

/// `splitlane wait --match <target> --until allowed [--timeout S]`: wait for
/// a person to answer whether the caller may send messages to a session they
/// opened.
///
/// Reads `drive` in `surface.status`, which is the server's answer about
/// this caller and that session. A session nobody was asked about yet is
/// waited on all the same: the question may be put by a `send` that has not
/// been made. One nobody will ever be asked about ends the wait at once -
/// there is no answer coming, and waiting out the timeout said only that
/// the time was up.
pub fn wait_allowed(
    client: &impl IpcTransport,
    target: &str,
    timeout: std::time::Duration,
) -> Result<i32, CliError> {
    let surface_id = resolve_target(client, target)?;
    let started = std::time::Instant::now();
    loop {
        let status = client
            .call("surface.status", json!({ "surface_id": surface_id }))
            .map_err(CliError::runtime)?;
        match status.get("drive").and_then(Value::as_str) {
            Some("allowed") => {
                println!("{surface_id}\tallowed");
                return Ok(EXIT_OK);
            }
            Some("declined") => {
                println!("{surface_id}\tdeclined");
                return Ok(EXIT_REFUSED);
            }
            Some("not_offered") => {
                println!("{surface_id}\tnot_offered");
                eprintln!(
                    "splitlane: nobody is asked about this session for this caller. A person \
                     is asked only when a session they opened sends to another agent session \
                     they opened, in the same project. A session the caller opened itself \
                     needs no leave: just send to it."
                );
                return Ok(EXIT_RUNTIME);
            }
            _ if started.elapsed() >= timeout => {
                println!("{surface_id}\ttimeout");
                return Ok(EXIT_TIMEOUT);
            }
            _ => std::thread::sleep(INTERRUPT_POLL),
        }
    }
}

/// How often the rail is read while a stop is being confirmed.
const INTERRUPT_POLL: std::time::Duration = std::time::Duration::from_millis(300);

/// How an interrupt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interrupt {
    /// The agent's own record says the turn was stopped.
    Interrupted,
    /// No turn was running. Nothing was sent.
    NotRunning,
    /// The key was sent and no record of a stop was seen in time.
    NotConfirmed,
}

impl Interrupt {
    fn word(self) -> &'static str {
        match self {
            Interrupt::Interrupted => "interrupted",
            Interrupt::NotRunning => "not_running",
            Interrupt::NotConfirmed => "not_confirmed",
        }
    }

    fn exit_code(self) -> i32 {
        match self {
            Interrupt::Interrupted => EXIT_OK,
            Interrupt::NotRunning => EXIT_RUNTIME,
            Interrupt::NotConfirmed => EXIT_TIMEOUT,
        }
    }
}

/// Whether the rail shows that the run standing at `baseline` was stopped.
///
/// `Some(true)` is the stop confirmed. `Some(false)` is a run that ended some
/// other way after the key went in - it finished on its own a moment before
/// the key arrived - and waiting longer will not change that. `None` is "not
/// yet".
fn stop_confirmed(rail: &Value, baseline: u64) -> Option<bool> {
    let runs_ended = rail.get("runs_ended").and_then(Value::as_u64).unwrap_or(0);
    if runs_ended <= baseline {
        return None;
    }
    Some(rail.get("last_outcome").and_then(Value::as_str) == Some("interrupted"))
}

/// `splitlane interrupt <target> [--timeout S] [--json]`: stop the turn a
/// session is in and leave the session open.
///
/// The stop is confirmed from the rail and not from the pane going quiet:
/// the agent's own file records it, and silence is what an agent that is
/// still generating looks like too.
pub fn interrupt(
    client: &impl IpcTransport,
    target: &str,
    timeout: std::time::Duration,
    json: bool,
) -> Result<i32, CliError> {
    let surface_id = resolve_target(client, target)?;
    let sent = super::reject_legacy_error(
        client
            .call("surface.interrupt", json!({ "surface_id": surface_id }))
            .map_err(call_error)?,
    )?;
    let baseline = sent.get("runs_ended").and_then(Value::as_u64).unwrap_or(0);
    let mut rail = Value::Null;
    let outcome = if sent.get("sent").and_then(Value::as_bool) != Some(true) {
        Interrupt::NotRunning
    } else {
        let started = std::time::Instant::now();
        loop {
            let status = client
                .call("surface.status", json!({ "surface_id": surface_id }))
                .map_err(CliError::runtime)?;
            rail = status.get("rail").cloned().unwrap_or(Value::Null);
            match stop_confirmed(&rail, baseline) {
                Some(true) => break Interrupt::Interrupted,
                Some(false) => break Interrupt::NotConfirmed,
                None if started.elapsed() >= timeout => break Interrupt::NotConfirmed,
                None => std::thread::sleep(INTERRUPT_POLL),
            }
        }
    };
    if json {
        super::print_json(&json!({
            "outcome": outcome.word(),
            "surface_id": surface_id,
            "status": rail.get("status").or_else(|| sent.get("rail_status_at_send")),
            "runs_ended": rail.get("runs_ended").or_else(|| sent.get("runs_ended")),
            "last_outcome": rail.get("last_outcome").or_else(|| sent.get("last_outcome")),
        }))?;
    } else {
        println!("{surface_id}\t{}", outcome.word());
    }
    Ok(outcome.exit_code())
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

    /// Answers the interrupt once and then a scripted run of rail readings.
    struct Interrupted {
        sent: Value,
        rails: RefCell<Vec<Value>>,
        calls: RefCell<Vec<String>>,
    }
    impl Interrupted {
        fn new(sent: Value, rails: Vec<Value>) -> Self {
            Self {
                sent,
                rails: RefCell::new(rails),
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl IpcTransport for Interrupted {
        fn call(&self, method: &str, _params: Value) -> Result<Value, String> {
            self.calls.borrow_mut().push(method.to_string());
            match method {
                "surface.list" => Ok(json!({ "surfaces": [
                    { "surface_id": 12, "name": "api-docs" },
                ]})),
                "surface.interrupt" => Ok(self.sent.clone()),
                _ => {
                    let mut rails = self.rails.borrow_mut();
                    let rail = if rails.len() > 1 {
                        rails.remove(0)
                    } else {
                        rails.first().cloned().unwrap_or(Value::Null)
                    };
                    Ok(json!({ "rail": rail }))
                }
            }
        }
    }

    const SOON: std::time::Duration = std::time::Duration::from_secs(5);

    #[test]
    fn a_stop_is_confirmed_by_the_run_ending_as_interrupted() {
        let fake = Interrupted::new(
            json!({ "sent": true, "runs_ended": 2, "rail_status_at_send": "running" }),
            vec![
                json!({ "status": "running", "runs_ended": 2, "last_outcome": "finished" }),
                json!({ "status": "idle", "runs_ended": 3, "last_outcome": "interrupted" }),
            ],
        );
        assert_eq!(
            interrupt(&fake, "api-docs", SOON, true).expect("ok"),
            EXIT_OK
        );
    }

    /// The outcome standing from the run before is not this run's.
    #[test]
    fn an_earlier_interrupt_does_not_confirm_this_one() {
        let earlier =
            json!({ "status": "running", "runs_ended": 2, "last_outcome": "interrupted" });
        assert_eq!(stop_confirmed(&earlier, 2), None);
        let fake = Interrupted::new(
            json!({ "sent": true, "runs_ended": 2, "rail_status_at_send": "running" }),
            vec![earlier],
        );
        assert_eq!(
            interrupt(&fake, "api-docs", std::time::Duration::ZERO, true).expect("ok"),
            EXIT_TIMEOUT
        );
    }

    /// The turn finished on its own a moment before the key arrived. That is
    /// said at once, not after the timeout: nothing more is coming.
    #[test]
    fn a_run_that_finished_by_itself_is_not_a_confirmed_stop() {
        let fake = Interrupted::new(
            json!({ "sent": true, "runs_ended": 2, "rail_status_at_send": "running" }),
            vec![json!({ "status": "idle", "runs_ended": 3, "last_outcome": "finished" })],
        );
        assert_eq!(
            interrupt(&fake, "api-docs", SOON, true).expect("ok"),
            EXIT_TIMEOUT
        );
        assert_eq!(fake.calls.borrow().len(), 3, "one reading was enough");
    }

    #[test]
    fn with_no_turn_running_nothing_is_waited_for() {
        let fake = Interrupted::new(
            json!({ "sent": false, "runs_ended": 2, "rail_status_at_send": "idle" }),
            Vec::new(),
        );
        assert_eq!(
            interrupt(&fake, "api-docs", SOON, true).expect("ok"),
            EXIT_RUNTIME
        );
        assert_eq!(
            *fake.calls.borrow(),
            ["surface.list", "surface.interrupt"],
            "the rail is not read"
        );
    }

    /// Answers `surface.status` with a scripted run of `drive` words.
    struct Driving {
        words: RefCell<Vec<Value>>,
    }
    impl IpcTransport for Driving {
        fn call(&self, method: &str, _params: Value) -> Result<Value, String> {
            if method == "surface.list" {
                return Ok(json!({ "surfaces": [{ "surface_id": 12, "name": "api" }] }));
            }
            let mut words = self.words.borrow_mut();
            let word = if words.len() > 1 {
                words.remove(0)
            } else {
                words.first().cloned().unwrap_or(Value::Null)
            };
            Ok(json!({ "surface_id": 12, "drive": word }))
        }
    }

    #[test]
    fn waiting_for_a_persons_answer_ends_on_the_answer() {
        let yes = Driving {
            words: RefCell::new(vec![json!("asked"), json!("asked"), json!("allowed")]),
        };
        assert_eq!(wait_allowed(&yes, "api", SOON).expect("ok"), EXIT_OK);
        let no = Driving {
            words: RefCell::new(vec![json!("asked"), json!("declined")]),
        };
        assert_eq!(wait_allowed(&no, "api", SOON).expect("ok"), EXIT_REFUSED);
        // No question can be put for this pair, so there is nothing to wait
        // out: the answer comes at once, with the timeout untouched.
        let never = Driving {
            words: RefCell::new(vec![json!("not_offered")]),
        };
        assert_eq!(wait_allowed(&never, "api", SOON).expect("ok"), EXIT_RUNTIME);
        // A question still standing, or never put, when the time is up.
        for word in [json!("asked"), Value::Null] {
            let pending = Driving {
                words: RefCell::new(vec![word]),
            };
            assert_eq!(
                wait_allowed(&pending, "api", std::time::Duration::ZERO).expect("ok"),
                EXIT_TIMEOUT
            );
        }
    }

    /// Asking a person is its own exit code: the caller neither succeeded nor
    /// was refused, and has to wait.
    #[test]
    fn a_write_that_asked_a_person_is_exit_9() {
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Err(
                "splitlane error -32005: surface.interrupt asked a person (asked_person): wait"
                    .to_string(),
            ),
        };
        let err = interrupt(&fake, "api-docs", SOON, false).expect_err("asked");
        assert_eq!(err.code, EXIT_ASKED_PERSON);
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Err(
                "splitlane error -32004: surface.interrupt refused (person_declined): no"
                    .to_string(),
            ),
        };
        let err = interrupt(&fake, "api-docs", SOON, false).expect_err("refused");
        assert_eq!(err.code, EXIT_REFUSED);
    }

    /// An agent with no known key is "use another method", not a rule the
    /// caller broke.
    #[test]
    fn an_agent_with_no_interrupt_key_is_exit_7() {
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Err(
                "splitlane error -32004: surface.interrupt refused (no_interrupt): no".to_string(),
            ),
        };
        let err = interrupt(&fake, "api-docs", SOON, false).expect_err("refused");
        assert_eq!(err.code, EXIT_NO_TURN_SIGNAL);
        let fake = Listed {
            calls: RefCell::new(Vec::new()),
            reply: Err(
                "splitlane error -32004: surface.interrupt refused (waiting): no".to_string(),
            ),
        };
        let err = interrupt(&fake, "api-docs", SOON, false).expect_err("refused");
        assert_eq!(err.code, EXIT_REFUSED);
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
