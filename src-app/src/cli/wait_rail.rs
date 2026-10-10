//! `splitlane wait --until turn-end` and `wait --state` - block on what the
//! rail says about a surface, rather than on its output going quiet.
//!
//! `wait --idle` reads silence, and silence is a poor sign that an agent's turn
//! is over: an agent generating its answer prints nothing for tens of seconds,
//! and one waiting on a permission prompt prints nothing at all. These two
//! modes read `surface.status`'s `rail` object instead - the same word the rail
//! row shows, decided from the agent's own file where one is read.
//!
//! The decision is a pure function over one observation of each target, so the
//! whole outcome table is tested without a socket. The loop around it is a
//! poll of `surface.status`; where the transport can tick a subscription, the
//! `surface.rail` event stream is used to wake that poll early, and where it
//! cannot (Windows named pipes) the poll runs on its own clock.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use splitlane_ipc_client::{IpcClient, IpcTransport, StreamEvent};

use super::selector::{resolve_all, resolve_target};
use super::wait_cmd::MatchMode;
use super::{
    CliError, EXIT_AGENT_FAILED, EXIT_INTERRUPTED, EXIT_NEEDS_PERSON, EXIT_NO_TURN_SIGNAL, EXIT_OK,
    EXIT_RUNTIME, EXIT_TIMEOUT,
};

/// How often `surface.status` is asked when no event wakes the loop sooner.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a surface that shows no turn in flight is given to start one, and
/// how long a surface with no turn signal is given to gain one.
///
/// The state pass samples every two seconds and an agent's first hook frame
/// arrives with its first prompt, so a `wait` issued right after a prompt was
/// sent can be looking at a surface that has not caught up yet. Ten seconds
/// covers a pass, a hook round trip and a cold start with room to spare.
pub const DEFAULT_START_GRACE: Duration = Duration::from_secs(10);

/// What the wait is for.
#[derive(Clone, Debug, PartialEq)]
pub enum RailWait {
    /// The turn in flight ends: a run is counted past the baseline and the
    /// surface is idle.
    TurnEnd,
    /// The status word is one of these.
    State(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct RailWaitOptions {
    pub wait: RailWait,
    /// `runs_ended` as it was before the prompt was sent. Without it the
    /// baseline is whatever the surface reports when the wait starts.
    pub after: Option<u64>,
    /// Keep waiting while the agent waits for a person instead of returning.
    pub through_waiting: bool,
    /// Do not take a turn's end for the end while a background command the
    /// agent started is still alive: it may be waiting on that command and
    /// about to carry on.
    pub settled: bool,
    pub start_grace: Duration,
    pub timeout: Duration,
    pub mode: MatchMode,
}

/// The status words `--state` accepts.
pub const STATUS_WORDS: [&str; 5] = ["starting", "running", "waiting", "idle", "failed"];

/// One reading of one surface.
#[derive(Clone, Debug, PartialEq)]
enum Observation {
    /// The surface is no longer there.
    Gone,
    /// The surface has no rail: a shell no agent hook has reported from.
    NoRail,
    Rail(Rail),
}

#[derive(Clone, Debug, PartialEq, Default)]
struct Rail {
    status: String,
    tier: String,
    runs_ended: u64,
    last_outcome: Option<String>,
    exited: bool,
    message: Option<String>,
    /// Absent from an instance older than the field, which reads as none.
    background_shells: u64,
}

impl Observation {
    fn from_status(value: &Value) -> Self {
        let Some(rail) = value.get("rail").filter(|rail| rail.is_object()) else {
            return Observation::NoRail;
        };
        let text = |key: &str| rail.get(key).and_then(Value::as_str).map(str::to_string);
        Observation::Rail(Rail {
            status: text("status").unwrap_or_default(),
            tier: text("tier").unwrap_or_default(),
            runs_ended: rail.get("runs_ended").and_then(Value::as_u64).unwrap_or(0),
            last_outcome: text("last_outcome"),
            exited: rail.get("exited").and_then(Value::as_bool).unwrap_or(false),
            message: text("message"),
            background_shells: rail
                .get("background_shells")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        })
    }
}

/// How one target's wait ended.
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    /// The turn ended and the agent is idle.
    Finished,
    /// The turn was stopped before it finished. The agent is idle, and there
    /// is no answer to read.
    Interrupted,
    /// `--state`: the status word is one of those asked for.
    Matched,
    /// The agent is waiting for a person. Not an answer and not an end.
    Waiting,
    /// The run failed, or the agent is gone.
    Failed,
    /// The surface closed.
    Closed,
    /// No turn was in flight and none started.
    NoTurn,
    /// The surface has no rail to wait on.
    NoRail,
    /// Nothing that speaks for this surface can report a turn ending.
    NoTurnSignal,
    /// A source that could report a turn ending stopped speaking mid-wait.
    Degraded,
    Timeout,
}

impl Outcome {
    fn wire_str(&self) -> &'static str {
        match self {
            Outcome::Finished => "finished",
            Outcome::Interrupted => "interrupted",
            Outcome::Matched => "matched",
            Outcome::Waiting => "waiting",
            Outcome::Failed => "failed",
            Outcome::Closed => "closed",
            Outcome::NoTurn => "no_turn",
            Outcome::NoRail => "no_rail",
            Outcome::NoTurnSignal => "no_turn_signal",
            Outcome::Degraded => "degraded",
            Outcome::Timeout => "timeout",
        }
    }

    fn exit_code(&self) -> i32 {
        match self {
            Outcome::Finished | Outcome::Matched => EXIT_OK,
            Outcome::Interrupted => EXIT_INTERRUPTED,
            Outcome::Waiting => EXIT_NEEDS_PERSON,
            Outcome::Failed => EXIT_AGENT_FAILED,
            Outcome::Closed | Outcome::NoTurn => EXIT_RUNTIME,
            Outcome::NoRail | Outcome::NoTurnSignal | Outcome::Degraded => EXIT_NO_TURN_SIGNAL,
            Outcome::Timeout => EXIT_TIMEOUT,
        }
    }

    /// Worst first, for the exit code of a wait over several surfaces: a
    /// failure outranks a question, which outranks a surface that could not be
    /// waited on, which outranks running out of time. A stopped turn comes
    /// last of the ones that are not success: every other outcome asks for
    /// something to be done first, and each surface's own word is in the
    /// report either way.
    fn severity(&self) -> u8 {
        match self {
            Outcome::Failed => 6,
            Outcome::Waiting => 5,
            Outcome::NoRail | Outcome::NoTurnSignal | Outcome::Degraded => 4,
            Outcome::Closed | Outcome::NoTurn => 3,
            Outcome::Timeout => 2,
            Outcome::Interrupted => 1,
            Outcome::Finished | Outcome::Matched => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Continue,
    Done(Outcome),
}

/// What the wait has learned about one target so far.
#[derive(Clone, Debug, Default)]
struct Track {
    /// `runs_ended` the turn being waited for must exceed.
    baseline: Option<u64>,
    /// A turn was seen in flight, so there is something to wait for.
    saw_turn: bool,
    /// A source that can report a turn ending has spoken during this wait.
    saw_turn_signal: bool,
    /// The last rail read, kept for the report.
    last: Option<Rail>,
}

/// Decide one target's `--until turn-end` wait from one observation.
///
/// The order of the checks is the contract:
///
/// 1. a closed surface or one with no rail ends the wait at once;
/// 2. a run counted past the baseline, with the surface idle, is the turn's
///    end - checked before anything about the source, because the count was
///    made by a source that could make it. How it ended is the rail's word
///    for that run: finished, failed, or stopped before it finished. With
///    `settled`, not while a background command of the agent's is alive -
///    the wait carries on until the session is idle with none left;
/// 3. a failed run or an agent that is gone is a failure;
/// 4. waiting for a person returns at once unless `through_waiting`, whatever
///    the source: a person being needed is not a claim about a turn;
/// 5. a surface nothing can report a turn ending for is refused - after the
///    grace when it never had such a source, at once when it lost one;
/// 6. a surface with no turn in flight is given the grace to start one, unless
///    the caller named the baseline, which is the proof that a prompt was sent.
///
/// Silence is never read as an end. A surface on the lowest tier is refused
/// rather than waited on, because the only thing its source can say is that
/// output is arriving.
fn decide_turn_end(
    observation: &Observation,
    track: &mut Track,
    options: &RailWaitOptions,
    elapsed: Duration,
) -> Step {
    let rail = match observation {
        Observation::Gone => return Step::Done(Outcome::Closed),
        Observation::NoRail => return Step::Done(Outcome::NoRail),
        Observation::Rail(rail) => rail,
    };
    track.last = Some(rail.clone());
    let explicit = options.after.is_some();
    let baseline = *track
        .baseline
        .get_or_insert(options.after.unwrap_or(rail.runs_ended));
    let status = rail.status.as_str();
    if matches!(status, "running" | "starting" | "waiting") {
        track.saw_turn = true;
    }
    let ended = rail.runs_ended > baseline;

    if ended && status == "idle" {
        // The turn is over and the agent may not be done: a command it left
        // running wakes it when it ends, and the answer comes after that.
        // Nothing says whether this one will ever end, so a session that
        // left a dev server behind is waited on until the timeout.
        if options.settled && rail.background_shells > 0 {
            return Step::Continue;
        }
        return Step::Done(match rail.last_outcome.as_deref() {
            Some("failed") => Outcome::Failed,
            Some("interrupted") => Outcome::Interrupted,
            _ => Outcome::Finished,
        });
    }
    // A failure standing from before a named baseline is the previous run's;
    // the prompt just sent has not been picked up yet.
    if rail.exited || (status == "failed" && (ended || !explicit)) {
        return Step::Done(Outcome::Failed);
    }
    // Before the tier: a question an agent asks ahead of its first prompt is
    // read off the pane's terminal, which is on the lowest tier, and a caller
    // told "no turn signal" about it would not know a person is needed.
    if status == "waiting" && !options.through_waiting {
        return Step::Done(Outcome::Waiting);
    }
    if rail.tier == "T3" {
        if track.saw_turn_signal {
            return Step::Done(Outcome::Degraded);
        }
        // `starting` is the launch still in progress; nothing has had the
        // chance to speak yet.
        if status == "starting" || elapsed < options.start_grace {
            return Step::Continue;
        }
        return Step::Done(Outcome::NoTurnSignal);
    }
    track.saw_turn_signal = true;
    if status == "waiting" {
        return Step::Continue;
    }
    if !explicit && !track.saw_turn && elapsed >= options.start_grace {
        return Step::Done(Outcome::NoTurn);
    }
    Step::Continue
}

/// Decide one target's `--state` wait: the raw mode, with no holds beyond the
/// ones already in the word itself.
fn decide_state(observation: &Observation, track: &mut Track, words: &[String]) -> Step {
    match observation {
        Observation::Gone => Step::Done(Outcome::Closed),
        Observation::NoRail => Step::Done(Outcome::NoRail),
        Observation::Rail(rail) => {
            track.last = Some(rail.clone());
            if words.contains(&rail.status) {
                Step::Done(Outcome::Matched)
            } else {
                Step::Continue
            }
        }
    }
}

struct Target {
    id: u64,
    track: Track,
    outcome: Option<Outcome>,
}

/// The wait over its whole target set.
struct Session {
    options: RailWaitOptions,
    targets: Vec<Target>,
}

impl Session {
    fn new(ids: Vec<u64>, options: RailWaitOptions) -> Self {
        Self {
            options,
            targets: ids
                .into_iter()
                .map(|id| Target {
                    id,
                    track: Track::default(),
                    outcome: None,
                })
                .collect(),
        }
    }

    /// Read every undecided target once and decide it. `Ok(true)` when the
    /// wait as a whole is over.
    fn step(&mut self, client: &impl IpcTransport, elapsed: Duration) -> Result<bool, CliError> {
        for target in &mut self.targets {
            if target.outcome.is_some() {
                continue;
            }
            let observation = observe(client, target.id)?;
            let step = match &self.options.wait {
                RailWait::TurnEnd => {
                    decide_turn_end(&observation, &mut target.track, &self.options, elapsed)
                }
                RailWait::State(words) => decide_state(&observation, &mut target.track, words),
            };
            if let Step::Done(outcome) = step {
                target.outcome = Some(outcome);
            }
        }
        Ok(self.is_done())
    }

    fn is_done(&self) -> bool {
        let decided = self.targets.iter().filter(|t| t.outcome.is_some()).count();
        match self.options.mode {
            MatchMode::Single | MatchMode::Any => decided > 0,
            MatchMode::All => decided == self.targets.len(),
        }
    }

    /// Close the wait: whatever is still undecided ran out of time.
    fn report(mut self) -> (Value, i32) {
        let any = self.options.mode == MatchMode::Any;
        if !any {
            for target in &mut self.targets {
                target.outcome.get_or_insert(Outcome::Timeout);
            }
        } else if self.targets.iter().all(|t| t.outcome.is_none()) {
            for target in &mut self.targets {
                target.outcome = Some(Outcome::Timeout);
            }
        }
        let decided: Vec<&Target> = self
            .targets
            .iter()
            .filter(|t| t.outcome.is_some())
            .collect();
        let worst = decided
            .iter()
            .filter_map(|t| t.outcome.as_ref())
            .max_by_key(|outcome| outcome.severity())
            .cloned()
            .unwrap_or(Outcome::Timeout);
        let targets: Vec<Value> = decided
            .iter()
            .map(|target| {
                let rail = target.track.last.as_ref();
                json!({
                    "surface_id": target.id,
                    "outcome": target.outcome.as_ref().map(Outcome::wire_str),
                    "status": rail.map(|r| r.status.as_str()),
                    "tier": rail.map(|r| r.tier.as_str()),
                    "runs_ended": rail.map(|r| r.runs_ended),
                    "last_outcome": rail.and_then(|r| r.last_outcome.as_deref()),
                    "background_shells": rail.map(|r| r.background_shells),
                    "message": rail.and_then(|r| r.message.as_deref()),
                })
            })
            .collect();
        (
            json!({ "outcome": worst.wire_str(), "targets": targets }),
            worst.exit_code(),
        )
    }
}

fn observe(client: &impl IpcTransport, id: u64) -> Result<Observation, CliError> {
    match client.call("surface.status", json!({ "surface_id": id })) {
        Ok(value) => {
            if let Some(message) = value.get("error").and_then(Value::as_str) {
                return if is_surface_gone(message) {
                    Ok(Observation::Gone)
                } else {
                    Err(CliError::runtime(message.to_string()))
                };
            }
            Ok(Observation::from_status(&value))
        }
        // A down instance is fatal: say so rather than report every surface
        // as closed.
        Err(e) if e.contains("unreachable") => Err(CliError::runtime(e)),
        Err(e) if is_surface_gone(&e) => Ok(Observation::Gone),
        Err(e) => Err(CliError::runtime(e)),
    }
}

fn is_surface_gone(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("not found") || lower.contains("-32602")
}

/// The wait on a clock of its own: read, decide, sleep. This is the whole of
/// the wait on a transport that cannot tick a subscription, and the fallback
/// everywhere else.
///
/// `now` and `sleep` are parameters so the loop is tested with a fake clock.
fn run_polling(
    client: &impl IpcTransport,
    mut session: Session,
    now: impl Fn() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<(Value, i32), CliError> {
    let timeout = session.options.timeout;
    loop {
        let elapsed = now();
        if session.step(client, elapsed)? || elapsed >= timeout {
            return Ok(session.report());
        }
        sleep(POLL_INTERVAL);
    }
}

/// `splitlane wait --match <sel> --until turn-end | --state <words>`.
pub fn wait_rail(
    client: &IpcClient,
    target: &str,
    options: RailWaitOptions,
) -> Result<i32, CliError> {
    let ids: Vec<u64> = match options.mode {
        MatchMode::Single => vec![resolve_target(client, target)?],
        MatchMode::Any | MatchMode::All => resolve_all(client, target)?,
    };
    let started = Instant::now();
    let timeout = options.timeout;
    let mut session = Session::new(ids.clone(), options);

    // The first reading is taken before subscribing, so a wait that is already
    // satisfied returns without opening a stream.
    if session.step(client, started.elapsed())? {
        return finish(session.report());
    }

    let socket = splitlane_ipc_client::resolve_socket_path();
    let params = json!({ "surfaces": ids, "types": [crate::rail_state::RAIL_EVENT_TYPE] });
    let mut failure: Option<CliError> = None;
    let mut closed = false;
    let mut session = Some(session);
    let streamed = match &socket {
        Some(socket) => {
            splitlane_ipc_client::subscribe_stream_timed(socket, params, POLL_INTERVAL, |event| {
                if matches!(event, StreamEvent::Closed) {
                    closed = true;
                    return false;
                }
                let Some(current) = session.as_mut() else {
                    return false;
                };
                // An event or a tick: either way the answer is in
                // `surface.status`, which also carries the question's text and
                // the tier - the event is only the reason to look now.
                let elapsed = started.elapsed();
                match current.step(client, elapsed) {
                    Ok(done) => !(done || elapsed >= timeout),
                    Err(e) => {
                        failure = Some(e);
                        false
                    }
                }
            })
        }
        None => Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
    };
    if let Some(failure) = failure {
        return Err(failure);
    }
    let Some(session) = session.take() else {
        return Err(CliError::runtime("wait ended without a verdict (internal)"));
    };
    match streamed {
        Ok(()) if closed => Err(CliError::runtime(
            "the Splitlane event stream closed before the wait finished (did Splitlane exit?)",
        )),
        Ok(()) => finish(session.report()),
        // No subscription to be woken by: the same wait, on its own clock.
        Err(_) => finish(run_polling(
            client,
            session,
            || started.elapsed(),
            std::thread::sleep,
        )?),
    }
}

fn finish((report, code): (Value, i32)) -> Result<i32, CliError> {
    super::print_json(&report)?;
    match report.get("outcome").and_then(Value::as_str) {
        Some("no_turn_signal" | "degraded") => eprintln!(
            "splitlane: nothing that reports for this surface can say a turn ended. \
             Use `wait --idle --pattern <sentinel>`, or `send --report-file` and wait for its sentinel."
        ),
        Some("no_rail") => eprintln!(
            "splitlane: this surface is not an agent session and no agent hook has reported from it."
        ),
        Some("no_turn") => eprintln!(
            "splitlane: no turn was in flight and none started. \
             Pass --after <runs_ended> taken before the prompt was sent."
        ),
        Some("waiting") => {
            eprintln!("splitlane: the agent is waiting for a person; answer it in its own pane.")
        }
        Some("interrupted") => eprintln!(
            "splitlane: the turn was stopped before it finished; there is no answer to read."
        ),
        _ => {}
    }
    Ok(code)
}

/// Parse `--state idle,waiting` into the words to wait for.
pub fn parse_state_words(raw: &str) -> Result<Vec<String>, CliError> {
    let words: Vec<String> = raw
        .split(',')
        .map(|word| word.trim().to_ascii_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() {
        return Err(CliError::runtime("wait --state needs at least one word"));
    }
    if let Some(unknown) = words.iter().find(|w| !STATUS_WORDS.contains(&w.as_str())) {
        return Err(CliError::runtime(format!(
            "unknown status word '{unknown}'; expected one of: {}",
            STATUS_WORDS.join(", ")
        )));
    }
    Ok(words)
}

/// The timeout and the grace, from the flags.
pub fn durations(timeout_secs: Option<u64>, start_grace_secs: Option<u64>) -> (Duration, Duration) {
    (
        timeout_secs.map_or(DEFAULT_TIMEOUT, Duration::from_secs),
        start_grace_secs.map_or(DEFAULT_START_GRACE, Duration::from_secs),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    fn options(after: Option<u64>) -> RailWaitOptions {
        RailWaitOptions {
            wait: RailWait::TurnEnd,
            after,
            through_waiting: false,
            settled: false,
            start_grace: DEFAULT_START_GRACE,
            timeout: DEFAULT_TIMEOUT,
            mode: MatchMode::Single,
        }
    }

    fn rail(status: &str, tier: &str, runs_ended: u64) -> Observation {
        Observation::Rail(Rail {
            status: status.to_string(),
            tier: tier.to_string(),
            runs_ended,
            ..Rail::default()
        })
    }

    const SOON: Duration = Duration::from_secs(1);
    const LATE: Duration = Duration::from_secs(11);

    fn decide(observation: &Observation, after: Option<u64>, elapsed: Duration) -> Step {
        decide_turn_end(observation, &mut Track::default(), &options(after), elapsed)
    }

    #[test]
    fn a_closed_surface_ends_the_wait() {
        assert_eq!(
            decide(&Observation::Gone, None, SOON),
            Step::Done(Outcome::Closed)
        );
        assert_eq!(Outcome::Closed.exit_code(), EXIT_RUNTIME);
    }

    #[test]
    fn a_surface_without_a_rail_is_refused_at_once() {
        assert_eq!(
            decide(&Observation::NoRail, None, SOON),
            Step::Done(Outcome::NoRail)
        );
        assert_eq!(Outcome::NoRail.exit_code(), EXIT_NO_TURN_SIGNAL);
    }

    /// The end of a turn is a run counted past the baseline **and** an idle
    /// surface. A count that moved while the agent is already working again is
    /// not an end to return on.
    #[test]
    fn a_counted_run_and_an_idle_surface_is_the_end_of_the_turn() {
        for tier in ["T1", "T2"] {
            assert_eq!(
                decide(&rail("idle", tier, 8), Some(7), SOON),
                Step::Done(Outcome::Finished),
                "{tier}"
            );
            assert_eq!(
                decide(&rail("running", tier, 8), Some(7), SOON),
                Step::Continue
            );
            assert_eq!(
                decide(&rail("idle", tier, 7), Some(7), LATE),
                Step::Continue
            );
        }
        assert_eq!(Outcome::Finished.exit_code(), EXIT_OK);
    }

    /// A stopped turn ended too, and left nothing to read: the caller that
    /// would go on to `answer` has to be told it is not that kind of end.
    #[test]
    fn a_turn_that_was_stopped_is_not_reported_as_finished() {
        let mut stopped = rail("idle", "T1", 8);
        if let Observation::Rail(rail) = &mut stopped {
            rail.last_outcome = Some("interrupted".to_string());
        }
        assert_eq!(
            decide(&stopped, Some(7), SOON),
            Step::Done(Outcome::Interrupted)
        );
        assert_eq!(Outcome::Interrupted.wire_str(), "interrupted");
        assert_eq!(Outcome::Interrupted.exit_code(), EXIT_INTERRUPTED);
        // The stop standing from before the baseline is the previous run's.
        let mut earlier = rail("idle", "T1", 7);
        if let Observation::Rail(rail) = &mut earlier {
            rail.last_outcome = Some("interrupted".to_string());
        }
        assert_eq!(decide(&earlier, Some(7), SOON), Step::Continue);
        // Over several surfaces anything that needs acting on outranks it.
        assert!(Outcome::Interrupted.severity() > Outcome::Finished.severity());
        assert!(Outcome::Interrupted.severity() < Outcome::Timeout.severity());
    }

    /// A turn that ended in an error ended, and is reported as a failure.
    #[test]
    fn a_turn_that_ended_failed_is_a_failure() {
        let mut ended = Rail {
            status: "idle".to_string(),
            tier: "T1".to_string(),
            runs_ended: 8,
            ..Rail::default()
        };
        ended.last_outcome = Some("failed".to_string());
        assert_eq!(
            decide(&Observation::Rail(ended), Some(7), SOON),
            Step::Done(Outcome::Failed)
        );
        assert_eq!(Outcome::Failed.exit_code(), EXIT_AGENT_FAILED);
    }

    #[test]
    fn a_failed_status_or_a_gone_agent_is_a_failure() {
        assert_eq!(
            decide(&rail("failed", "T1", 8), Some(7), SOON),
            Step::Done(Outcome::Failed)
        );
        assert_eq!(
            decide(&rail("failed", "T2", 3), None, SOON),
            Step::Done(Outcome::Failed)
        );
        let gone = Observation::Rail(Rail {
            status: "idle".to_string(),
            tier: "T1".to_string(),
            runs_ended: 7,
            exited: true,
            ..Rail::default()
        });
        assert_eq!(decide(&gone, Some(7), SOON), Step::Done(Outcome::Failed));
    }

    /// A failure left standing by the run before the one being waited for is
    /// not this run's: the prompt just sent has not been picked up yet.
    #[test]
    fn a_failure_from_before_the_baseline_is_not_this_runs() {
        assert_eq!(
            decide(&rail("failed", "T1", 7), Some(7), SOON),
            Step::Continue
        );
    }

    /// Waiting for a person is returned at once, with the question, and is
    /// neither an answer nor an end.
    #[test]
    fn waiting_for_a_person_returns_at_once() {
        let asking = Observation::Rail(Rail {
            status: "waiting".to_string(),
            tier: "T1".to_string(),
            runs_ended: 7,
            message: Some("Allow Bash(rm -rf build)?".to_string()),
            ..Rail::default()
        });
        assert_eq!(decide(&asking, Some(7), SOON), Step::Done(Outcome::Waiting));
        assert_eq!(Outcome::Waiting.exit_code(), EXIT_NEEDS_PERSON);
    }

    /// An agent asking whether to trust the folder has no file and no hook
    /// frame yet, so the word comes from the lowest tier. It is still a
    /// person being needed, and not "no turn signal".
    #[test]
    fn a_question_before_the_first_prompt_is_returned_on_the_lowest_tier() {
        assert_eq!(
            decide(&rail("waiting", "T3", 0), Some(0), SOON),
            Step::Done(Outcome::Waiting)
        );
        assert_eq!(
            decide(&rail("waiting", "T3", 0), Some(0), LATE),
            Step::Done(Outcome::Waiting)
        );
    }

    /// The default is unchanged: a turn that ended is the end, whatever the
    /// agent left running.
    #[test]
    fn a_turn_end_beside_a_background_command_still_returns_by_default() {
        let mut waiting_on_tests = rail("idle", "T1", 8);
        if let Observation::Rail(rail) = &mut waiting_on_tests {
            rail.background_shells = 1;
        }
        assert_eq!(
            decide(&waiting_on_tests, Some(7), SOON),
            Step::Done(Outcome::Finished)
        );
    }

    /// With `--settled` an agent waiting on its own test run is not done: the
    /// wait goes through the turn that follows the command and returns when
    /// the session is idle with nothing left alive.
    #[test]
    fn settled_waits_out_a_background_command() {
        let shells = |status: &str, runs: u64, shells: u64| {
            let mut observation = rail(status, "T1", runs);
            if let Observation::Rail(rail) = &mut observation {
                rail.background_shells = shells;
            }
            observation
        };
        let mut opts = options(Some(7));
        opts.settled = true;
        let mut track = Track::default();
        for (observation, expected) in [
            (shells("running", 7, 0), Step::Continue),
            // The turn ended with the test run alive.
            (shells("idle", 8, 1), Step::Continue),
            // The run ended and woke the agent.
            (shells("running", 8, 0), Step::Continue),
            (shells("idle", 9, 0), Step::Done(Outcome::Finished)),
        ] {
            assert_eq!(
                decide_turn_end(&observation, &mut track, &opts, LATE),
                expected,
                "{observation:?}"
            );
        }
    }

    /// With `--through-waiting` the wait carries on: the person answers in the
    /// agent's own pane and the turn continues.
    #[test]
    fn through_waiting_carries_on_past_a_question() {
        let mut opts = options(Some(7));
        opts.through_waiting = true;
        let mut track = Track::default();
        assert_eq!(
            decide_turn_end(&rail("waiting", "T1", 7), &mut track, &opts, SOON),
            Step::Continue
        );
        assert_eq!(
            decide_turn_end(&rail("running", "T1", 7), &mut track, &opts, SOON),
            Step::Continue
        );
        assert_eq!(
            decide_turn_end(&rail("idle", "T1", 8), &mut track, &opts, SOON),
            Step::Done(Outcome::Finished)
        );
    }

    /// Nothing that speaks for a `T3` surface can say a turn ended, so the
    /// wait is refused rather than answered from silence. The grace is for a
    /// surface whose first hook frame is still on its way.
    #[test]
    fn the_lowest_tier_is_refused_after_the_grace() {
        for status in ["idle", "running"] {
            assert_eq!(
                decide(&rail(status, "T3", 0), Some(0), SOON),
                Step::Continue,
                "{status}, inside the grace"
            );
            assert_eq!(
                decide(&rail(status, "T3", 0), Some(0), LATE),
                Step::Done(Outcome::NoTurnSignal),
                "{status}, past it"
            );
        }
        // A launch still in progress is not refused: nothing has spoken yet.
        assert_eq!(
            decide(&rail("starting", "T3", 0), None, LATE),
            Step::Continue
        );
        assert_eq!(Outcome::NoTurnSignal.exit_code(), EXIT_NO_TURN_SIGNAL);
    }

    /// A surface that gains a source inside the grace is waited on like any
    /// other.
    #[test]
    fn a_surface_that_gains_a_source_is_waited_on() {
        let opts = options(Some(0));
        let mut track = Track::default();
        assert_eq!(
            decide_turn_end(&rail("idle", "T3", 0), &mut track, &opts, SOON),
            Step::Continue
        );
        assert_eq!(
            decide_turn_end(&rail("running", "T2", 0), &mut track, &opts, LATE),
            Step::Continue
        );
        assert_eq!(
            decide_turn_end(&rail("idle", "T2", 1), &mut track, &opts, LATE),
            Step::Done(Outcome::Finished)
        );
    }

    /// A source that stops speaking mid-wait ends the wait at once, with no
    /// grace: waiting on would be waiting on silence.
    #[test]
    fn losing_the_source_mid_wait_is_degraded() {
        let opts = options(Some(7));
        let mut track = Track::default();
        assert_eq!(
            decide_turn_end(&rail("running", "T1", 7), &mut track, &opts, SOON),
            Step::Continue
        );
        assert_eq!(
            decide_turn_end(&rail("running", "T3", 7), &mut track, &opts, SOON),
            Step::Done(Outcome::Degraded)
        );
        assert_eq!(Outcome::Degraded.exit_code(), EXIT_NO_TURN_SIGNAL);
    }

    /// A run counted before the source was lost is still a counted run.
    #[test]
    fn a_run_counted_before_the_source_was_lost_still_counts() {
        assert_eq!(
            decide(&rail("idle", "T3", 8), Some(7), LATE),
            Step::Done(Outcome::Finished)
        );
    }

    /// Without `--after`, a surface with a turn in flight is waited on until
    /// that turn ends.
    #[test]
    fn without_a_baseline_a_turn_in_flight_is_waited_for() {
        let opts = options(None);
        let mut track = Track::default();
        assert_eq!(
            decide_turn_end(&rail("running", "T1", 4), &mut track, &opts, SOON),
            Step::Continue
        );
        assert_eq!(
            decide_turn_end(&rail("idle", "T1", 4), &mut track, &opts, LATE),
            Step::Continue,
            "the finish is still being confirmed"
        );
        assert_eq!(
            decide_turn_end(&rail("idle", "T1", 5), &mut track, &opts, LATE),
            Step::Done(Outcome::Finished)
        );
    }

    /// Without `--after`, an idle surface is given the grace to start a turn.
    /// Waiting on for the end of a turn nobody has shown to have begun would be
    /// a wait with nothing behind it.
    #[test]
    fn without_a_baseline_an_idle_surface_gets_the_grace_and_no_more() {
        let opts = options(None);
        let mut track = Track::default();
        assert_eq!(
            decide_turn_end(&rail("idle", "T1", 4), &mut track, &opts, SOON),
            Step::Continue
        );
        assert_eq!(
            decide_turn_end(&rail("idle", "T1", 4), &mut track, &opts, LATE),
            Step::Done(Outcome::NoTurn)
        );
        assert_eq!(Outcome::NoTurn.exit_code(), EXIT_RUNTIME);

        // A turn too short to be seen running is still counted inside it.
        let mut track = Track::default();
        decide_turn_end(&rail("idle", "T1", 4), &mut track, &opts, SOON);
        assert_eq!(
            decide_turn_end(&rail("idle", "T1", 5), &mut track, &opts, SOON),
            Step::Done(Outcome::Finished)
        );
    }

    /// A named baseline is the proof a prompt was sent, so no grace applies.
    #[test]
    fn a_named_baseline_waits_past_the_grace() {
        assert_eq!(
            decide(&rail("idle", "T1", 7), Some(7), LATE),
            Step::Continue
        );
    }

    #[test]
    fn state_mode_matches_the_word_on_any_tier() {
        let words = vec!["idle".to_string(), "waiting".to_string()];
        let mut track = Track::default();
        assert_eq!(
            decide_state(&rail("running", "T3", 0), &mut track, &words),
            Step::Continue
        );
        assert_eq!(
            decide_state(&rail("waiting", "T1", 0), &mut track, &words),
            Step::Done(Outcome::Matched)
        );
        assert_eq!(
            decide_state(&Observation::NoRail, &mut track, &words),
            Step::Done(Outcome::NoRail)
        );
        assert_eq!(
            decide_state(&Observation::Gone, &mut track, &words),
            Step::Done(Outcome::Closed)
        );
    }

    #[test]
    fn state_words_are_checked() {
        assert_eq!(
            parse_state_words("Idle, waiting").expect("valid"),
            vec!["idle".to_string(), "waiting".to_string()]
        );
        assert!(parse_state_words("done").is_err());
        assert!(parse_state_words(" , ").is_err());
    }

    #[test]
    fn a_status_without_a_rail_object_is_no_rail() {
        assert_eq!(
            Observation::from_status(&json!({ "surface_id": 1, "state": "idle", "rail": null })),
            Observation::NoRail
        );
        // An instance too old to send the object at all.
        assert_eq!(
            Observation::from_status(&json!({ "surface_id": 1, "state": "idle" })),
            Observation::NoRail
        );
    }

    /// A scripted `surface.status` per surface: each call takes the next
    /// reading, and the last one repeats. No socket and no real sleep - the
    /// same shape as the fake behind `wait --idle`'s polling path.
    struct FakeRail {
        readings: RefCell<HashMap<u64, Vec<Option<Value>>>>,
        calls: Cell<u64>,
    }

    impl FakeRail {
        fn new(readings: Vec<(u64, Vec<Option<Value>>)>) -> Self {
            Self {
                readings: RefCell::new(readings.into_iter().collect()),
                calls: Cell::new(0),
            }
        }
    }

    impl IpcTransport for FakeRail {
        fn call(&self, method: &str, params: Value) -> Result<Value, String> {
            assert_eq!(method, "surface.status");
            self.calls.set(self.calls.get() + 1);
            let id = params["surface_id"].as_u64().expect("surface_id");
            let mut readings = self.readings.borrow_mut();
            let queue = readings.get_mut(&id).expect("a scripted surface");
            let next = if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue.first().cloned().flatten()
            };
            next.ok_or_else(|| format!("splitlane error -32602: surface_id {id} not found"))
        }
    }

    fn status(word: &str, tier: &str, runs_ended: u64) -> Option<Value> {
        Some(json!({
            "surface_id": 1,
            "state": "idle",
            "rail": {
                "status": word,
                "tier": tier,
                "runs_ended": runs_ended,
                "last_outcome": null,
                "exited": false,
                "message": if word == "waiting" { json!("Allow?") } else { Value::Null },
            }
        }))
    }

    /// Drive the polling path with a clock that advances only when the loop
    /// sleeps.
    fn poll(fake: &FakeRail, ids: Vec<u64>, opts: RailWaitOptions) -> (Value, i32) {
        let clock = Cell::new(Duration::ZERO);
        run_polling(
            fake,
            Session::new(ids, opts),
            || clock.get(),
            |slept| clock.set(clock.get() + slept),
        )
        .expect("the wait ran")
    }

    /// The path a transport that cannot tick a subscription takes: one
    /// `surface.status` per poll, the same decision, the same report.
    #[test]
    fn polling_reaches_the_end_of_a_turn() {
        let fake = FakeRail::new(vec![(
            1,
            vec![
                status("running", "T1", 7),
                status("running", "T1", 7),
                status("idle", "T1", 8),
            ],
        )]);
        let (report, code) = poll(&fake, vec![1], options(Some(7)));
        assert_eq!(code, EXIT_OK);
        assert_eq!(report["outcome"], "finished");
        assert_eq!(report["targets"][0]["runs_ended"], 8);
        assert_eq!(fake.calls.get(), 3, "one status read per poll");
    }

    #[test]
    fn polling_times_out_without_reading_silence_as_an_end() {
        let mut opts = options(Some(7));
        opts.timeout = Duration::from_secs(2);
        let fake = FakeRail::new(vec![(1, vec![status("running", "T1", 7)])]);
        let (report, code) = poll(&fake, vec![1], opts);
        assert_eq!(code, EXIT_TIMEOUT);
        assert_eq!(report["outcome"], "timeout");
    }

    #[test]
    fn polling_reports_a_surface_that_closed() {
        let fake = FakeRail::new(vec![(1, vec![status("running", "T1", 7), None])]);
        let (report, code) = poll(&fake, vec![1], options(Some(7)));
        assert_eq!(code, EXIT_RUNTIME);
        assert_eq!(report["outcome"], "closed");
    }

    /// `--all` waits for every target to reach an outcome of its own and
    /// reports each; the exit code is the worst of them.
    #[test]
    fn all_reports_every_target_and_exits_with_the_worst() {
        let mut opts = options(Some(0));
        opts.mode = MatchMode::All;
        let fake = FakeRail::new(vec![
            (1, vec![status("running", "T1", 0), status("idle", "T1", 1)]),
            (2, vec![status("waiting", "T1", 0)]),
            (
                3,
                vec![
                    status("running", "T2", 0),
                    status("running", "T2", 0),
                    status("failed", "T2", 1),
                ],
            ),
        ]);
        let (report, code) = poll(&fake, vec![1, 2, 3], opts);
        assert_eq!(code, EXIT_AGENT_FAILED);
        assert_eq!(report["outcome"], "failed");
        let outcomes: Vec<&str> = report["targets"]
            .as_array()
            .expect("targets")
            .iter()
            .map(|t| t["outcome"].as_str().expect("outcome"))
            .collect();
        assert_eq!(outcomes, ["finished", "waiting", "failed"]);
        assert_eq!(report["targets"][1]["message"], "Allow?");
    }

    /// `--any` returns on the first target to reach an outcome and reports
    /// only what was decided.
    #[test]
    fn any_returns_on_the_first_outcome() {
        let mut opts = options(Some(0));
        opts.mode = MatchMode::Any;
        let fake = FakeRail::new(vec![
            (1, vec![status("running", "T1", 0)]),
            (2, vec![status("running", "T1", 0), status("idle", "T1", 1)]),
        ]);
        let (report, code) = poll(&fake, vec![1, 2], opts);
        assert_eq!(code, EXIT_OK);
        assert_eq!(report["targets"].as_array().expect("targets").len(), 1);
        assert_eq!(report["targets"][0]["surface_id"], 2);
    }

    #[test]
    fn all_that_runs_out_of_time_reports_who_was_still_running() {
        let mut opts = options(Some(0));
        opts.mode = MatchMode::All;
        opts.timeout = Duration::from_secs(1);
        let fake = FakeRail::new(vec![
            (1, vec![status("idle", "T1", 1)]),
            (2, vec![status("running", "T1", 0)]),
        ]);
        let (report, code) = poll(&fake, vec![1, 2], opts);
        assert_eq!(code, EXIT_TIMEOUT);
        assert_eq!(report["targets"][0]["outcome"], "finished");
        assert_eq!(report["targets"][1]["outcome"], "timeout");
    }

    #[test]
    fn polling_serves_the_state_mode_too() {
        let mut opts = options(None);
        opts.wait = RailWait::State(vec!["waiting".to_string()]);
        let fake = FakeRail::new(vec![(
            1,
            vec![status("running", "T3", 0), status("waiting", "T1", 0)],
        )]);
        let (report, code) = poll(&fake, vec![1], opts);
        assert_eq!(code, EXIT_OK);
        assert_eq!(report["outcome"], "matched");
        assert_eq!(report["targets"][0]["status"], "waiting");
    }
}
