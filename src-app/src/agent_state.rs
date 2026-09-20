//! What an agent is doing, derived from the file it writes for itself.
//!
//! The rail needs three answers about every agent surface: working, stopped, or
//! waiting for a person. Until now all three came from `ai.*` hook frames. This
//! module is the other road: the CLI's own transcript, plus - for the cases the
//! transcript cannot settle - whether the agent has spawned a worker.
//!
//! # Why the transcript can answer at all
//!
//! Measured, not assumed:
//!
//! - the file is written **incrementally** - a `tool_use` line is on disk while
//!   its call is still running, proved by self-test;
//! - it is **only ever appended to**, across `--resume` and across compaction,
//!   so one session is one file and tailing it by offset is safe;
//! - during a permission wait **nothing at all** is written: the call's line and
//!   then, whenever the answer comes, its result.
//!
//! So a call with no result is an observable state, and the question is only
//! what that state means.
//!
//! # Three kinds of tool, because they fail differently
//!
//! The first cut was "instant tools versus everything else", and measuring the
//! process tree showed it was one distinction short. What actually decides how
//! a hanging call can be read is **how the tool executes**:
//!
//! - **In-process and quick.** Over 9 418 measured call/result pairs a `Write`
//!   never took 2.8 s and an `Edit` passed three seconds once in 633. One of
//!   these hanging is a question waiting for a person - which is exactly the
//!   scope the design chose to intercept, reached without a hook.
//! - **By spawning a process.** `Bash` runs past ten seconds in a fifth of its
//!   calls, so the clock says nothing - but the worker does. Measured on a live
//!   agent: while a command runs, a `zsh` appears under the agent. So a `Bash`
//!   call open with no such child is a command that has not started, which is
//!   what waiting for an answer looks like.
//!
//!   The second half of that sentence used to read "and while none runs it has
//!   **no children at all**", and it is false - see [`Worker::Absent`]. Two
//!   kinds of child sit under an idle agent, and the question this module asks
//!   its second input cannot stay "has any child" because of it.
//! - **In-process and unbounded.** An `mcp__…` call, a web fetch, a subagent.
//!   These run inside the agent, so they spawn nothing, and they can take
//!   minutes legitimately. Neither signal separates work from waiting here, so
//!   this module never claims either about them.
//!
//! # The prior is an allowlist, and that is deliberate
//!
//! [`execution_of`] recognises only tool names this build has **measured**, and
//! everything else - including every name that does not exist yet - is
//! [`ToolExecution::Opaque`], the kind about which nothing is claimed. A tool
//! from the next release of an agent CLI therefore cannot default to reporting
//! that a person is being kept waiting. That property is what makes this road
//! worth preferring to hooks, and it is worth more than the accuracy it costs.

// The rule and its shapes are built ahead of the surface that consumes them,
// deliberately and for the same reason `permission_grants` was: this decides
// what the rail tells a user about an agent they cannot see, and it is far
// easier to argue about, and to test, away from the pixels. The reader that
// fills `TranscriptProbe` is live in `claude_sessions::probe_state_from_tail`.
#![allow(
    dead_code,
    reason = "consumed by the rail's state pass, which lands next"
)]

use std::time::Duration;

use crate::ai_types::AgentState;

/// One tool call seen started and not yet seen finished.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenCall {
    /// The tool's name as the agent wrote it.
    pub(crate) name: String,
    /// How long it has been open, when the record carried a usable timestamp.
    ///
    /// `None` is not "new" - it is "the file did not say", and the rule treats
    /// it as no evidence at all. Reading a missing stamp as zero would date the
    /// call to 1970 and make it instantly older than any grace, so a single
    /// schema change moving that field would have had every session reporting a
    /// person kept waiting from the very first poll.
    pub(crate) age: Option<Duration>,
    /// How this call's tool executes, decided by the reader that produced it.
    ///
    /// # Why the reader decides and not the rule
    ///
    /// The rule below is agent-neutral and stays that way; the **vocabulary**
    /// is not portable at all. Claude Code's names are `Read`/`Write`/`Edit`/
    /// `Bash`; Codex's is one word, `exec_command` (7 of 7 calls in the local
    /// corpus - neither `shell` nor `apply_patch` appears, whatever older notes
    /// say). Worse, Codex's kind is not a function of the name at all: the same
    /// `exec_command` is [`ToolExecution::Spawns`] when its arguments carry
    /// `"sandbox_permissions":"require_escalated"` and [`ToolExecution::Opaque`]
    /// when they do not, because only the escalated form can ever stop for a
    /// person. A `&str -> ToolExecution` map cannot express that.
    ///
    /// So each reader states the kind for the calls it produces, and both feed
    /// one [`classify`].
    pub(crate) execution: ToolExecution,
}

/// One API message's cost and the moment it was billed.
///
/// Collected by the same tail read that answers what the agent is doing, and
/// it rides in [`TranscriptProbe`] for that reason alone: the read is already
/// there, bounded and off the render thread, and a second one every two
/// seconds per surface would be the expensive way to learn a cheap fact. It is
/// not a state signal and nothing in [`classify`] looks at it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpendSample {
    /// Unix seconds the record was written.
    pub(crate) at: i64,
    /// US dollars, from this codebase's own pricing table.
    pub(crate) dollars: f64,
}

/// How fast a surface is spending, and over how long that was measured.
///
/// The span is reported rather than assumed because it is often shorter than
/// the window asked for: the tail is 512 KB, so on a busy session it reaches
/// back tens of minutes and on a fresh one only as far as the session goes.
/// A caller that needs a trend before it acts can ask how long this was
/// watched instead of trusting a number built from one message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpendRate {
    pub(crate) dollars_per_minute: f64,
    /// The span the rate was actually divided by, in minutes.
    pub(crate) over_minutes: f64,
    /// How many API messages went into it.
    pub(crate) messages: usize,
}

/// Below this the divisor is noise rather than a span, and a rate is a claim
/// about a trend.
const MIN_SPEND_SPAN_SECS: i64 = 60;

/// The rate over the last `window_secs`, or `None` when nothing was observed.
///
/// **`None` is "not measured", never "spending nothing".** A surface whose
/// model this build cannot price, one whose tail carries no assistant record
/// yet, and one that has been watched for forty seconds all answer `None` -
/// and each of them may be spending steadily. The same distinction the rest of
/// this module keeps between silence and an answer.
///
/// The span is `now` minus the oldest sample in range rather than the window
/// itself, so a session two minutes old reports a rate over two minutes
/// instead of a tenth of the truth over ten.
pub(crate) fn spend_rate(samples: &[SpendSample], now: i64, window_secs: i64) -> Option<SpendRate> {
    let floor = now.saturating_sub(window_secs);
    // A record stamped in the future is a clock that moved, not a cost.
    let in_range: Vec<&SpendSample> = samples
        .iter()
        .filter(|s| s.at > floor && s.at <= now)
        .collect();
    let oldest = in_range.iter().map(|s| s.at).min()?;
    let span = now.saturating_sub(oldest);
    if span < MIN_SPEND_SPAN_SECS {
        return None;
    }
    let dollars: f64 = in_range.iter().map(|s| s.dollars).sum();
    let over_minutes = span as f64 / 60.0;
    Some(SpendRate {
        dollars_per_minute: dollars / over_minutes,
        over_minutes,
        messages: in_range.len(),
    })
}

/// What one read of a transcript's tail found.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TranscriptProbe {
    /// Calls started and not answered, oldest first.
    pub(crate) open_calls: Vec<OpenCall>,
    /// What the tail cost, one entry per API message. See [`SpendSample`] for
    /// why a spend fact rides in a state probe. Empty for every agent whose
    /// transcript this build cannot price.
    pub(crate) spend: Vec<SpendSample>,
    /// How long ago the newest record of a turn that has **not ended** was
    /// written, or `None` when the file shows no turn in flight.
    ///
    /// # The gap this closes, which is the commonest missing dot there is
    ///
    /// Everything else in this probe is about a *tool call*, and an agent is
    /// not always in one. Between the prompt and the first call, between a
    /// result and the next call, and for the whole of the final answer, the
    /// agent is **generating** - and generation appends nothing. The rule read
    /// that silence as an empty call set and answered `Finished`, so a session
    /// visibly working said `idle`.
    ///
    /// Measured in real transcripts, 31 August: a 41-second gap
    /// between an `assistant` record and the next one in this repository's
    /// session, an 18-second gap in another project's session - one false `idle` each,
    /// and the screenshot that reported this was taken inside the first.
    ///
    /// # What makes it a positive signal rather than an inference
    ///
    /// Claude Code stamps every `assistant` record with the API's own
    /// `stop_reason`: `tool_use` while the turn continues, `end_turn` when it
    /// is over. So the file *states* whether the turn is finished, and this is
    /// read rather than guessed - which is the same standard the rest of this
    /// module is held to, and it asks the vendor for nothing they were not
    /// already writing for themselves.
    ///
    /// # Why it can only ever say "working"
    ///
    /// It is consulted only where the call set is empty, so it cannot mask a
    /// waiting answer; and it votes for [`AgentState::Thinking`] alone, never
    /// for [`AgentState::WaitingForInput`]. A turn in flight with no call open
    /// is either the model generating or a permission ask the file cannot see -
    /// and between those two, "working" is the cheap mistake.
    ///
    /// An undatable turn is dropped rather than trusted: nothing bounds a turn
    /// whose age cannot be taken, and a missing dot is the direction this
    /// module chooses every time.
    pub(crate) open_turn: Option<Duration>,
    /// The session's newest record is an explicit failure (`system` with
    /// `subtype: "api_error"` or `"model_refusal_fallback"`). The transcript
    /// states these rather than leaving them to be inferred.
    pub(crate) errored: bool,
    /// Part of the window could not be read, so [`Self::open_calls`] may be
    /// missing a closure and cannot be trusted.
    ///
    /// This is not rare and it is not a corrupt file. A record carrying a large
    /// `Read` result or a long command's output routinely exceeds the per-line
    /// cap on agent-written JSONL - 838 such lines in the fifteen largest
    /// transcripts here, the biggest 1.33 MB - and a capped read leaves a
    /// fragment that must not be parsed. The call it would have closed then
    /// stays open forever, and the rule would have called an ordinary big read
    /// a person being kept waiting.
    ///
    /// So the flag exists to make the reader admit what it missed instead of
    /// inventing a state from it.
    pub(crate) incomplete: bool,
}

impl TranscriptProbe {
    /// Whether the file shows nothing in flight **at all** - accounted for or
    /// not.
    ///
    /// A different question from "does [`classify`] say `Finished`", and the
    /// difference is the point: `classify` reasons over calls recent enough to
    /// mean something, so a file frozen with an ancient one is finished to it
    /// and not empty to this. `app::agent_state_pass` takes the worker baseline
    /// on **this**, because a baseline taken while something is running absorbs
    /// it into the resting shape and hides it as a worker for the session.
    ///
    /// It exists as a name rather than as `probe.open_calls.is_empty()` written
    /// at the call site so that the two emptiness questions are stated once
    /// each, in one file, instead of being derived twice and held together by a
    /// comment - which is how they would quietly drift the day someone tunes
    /// the horizon.
    ///
    /// [`Self::open_turn`] is deliberately **not** asked here. A turn in flight
    /// with no call open is the agent generating, and generation spawns
    /// nothing - so the children under it at that moment really are its resting
    /// shape, which is exactly what a baseline is for.
    pub(crate) fn nothing_in_flight(&self) -> bool {
        self.open_calls.is_empty()
    }
}

/// Whether the agent has a worker process under it.
///
/// A narrow, factual question on purpose. The obvious alternative - "is the
/// agent burning CPU" - was measured and is useless: with no tool running at
/// all the agent showed anywhere from 0.4% to 36%, because thinking is work
/// too. Child processes separate cleanly where CPU does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Worker {
    /// At least one child process. Measured on a live agent: while a command
    /// runs, the shell it runs in is among them.
    Present,
    /// No children.
    ///
    /// **This is not reliably the agent's resting shape, and the code that
    /// asks the question must not assume it is.** The claim used to read "it
    /// keeps no helper processes of its own"; measured against Claude Code
    /// 2.1.246 on 26 August it is false twice over:
    ///
    /// - a **stdio MCP server** is a child of the agent for the whole session,
    ///   so a user who has one configured never reaches this variant at all -
    ///   and `splitlane mcp install` configures one;
    /// - `caffeinate` is spawned when a prompt is submitted and held for about
    ///   37 s, on a profile with no MCP servers whatsoever.
    ///
    /// Both make the rule below go quiet rather than lie - every arm that could
    /// report a person waiting needs this variant, and without it everything
    /// reads as `Thinking`. That is the safe direction under the invariant, and
    /// it is still a feature silently absent for a whole class of users. The
    /// fix is to stop asking "has any child" and ask "has a
    /// child that was not here when the turn ended".
    Absent,
    /// Not asked, or asked on a platform that cannot answer.
    Unknown,
}

/// Whether the agent's own process is where the pane says it should be.
///
/// Separate from [`Worker`] on purpose, and the separation was bought with a
/// live measurement. The
/// pass has always known this - it resolves the agent under the pane's PTY
/// child to reach the worker question at all - but it used to fold a failure
/// into [`Worker::Unknown`], which is not the same fact and does not defend the
/// same thing. `Unknown` says "the worker question has no answer"; the
/// [`ToolExecution::Instant`] arm treats that as a refutation that did not
/// come, and reports a person waiting. When the agent is **dead**, that reading
/// is a lie with nothing behind it: the file's open call is a frozen record of
/// a session that no longer exists, and the only process that could have closed
/// it is what went away.
///
/// Measured on 27 August: a `Write` permission ask, the agent SIGKILLed under
/// it, the transcript frozen with the call open - and the rail said "waiting
/// for you" for as long as it was watched, with the `Spawns` arm's guard doing
/// nothing because `Write` is not `Spawns`. One fact, asked once, now closes
/// both arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentProcess {
    /// Found under the pane's PTY child, so the file's account is about a
    /// session that still exists.
    Found,
    /// Not found - dead, reparented out of the subtree, or a launch chain this
    /// build does not recognise. Never read as "the agent is gone", only as
    /// "not seen": nothing about a person may be claimed from it.
    NotSeen,
}

/// How a tool does its work, which is what decides how a hanging call reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolExecution {
    /// Inside the agent, and measured to return in about a second.
    Instant,
    /// By spawning a process, so a worker child is proof the call started.
    Spawns,
    /// Inside the agent, for an unbounded time - a network call, a subagent.
    /// Neither the clock nor the process tells work from waiting.
    Opaque,
}

/// How long a call may hang before the file's silence starts to mean something.
///
/// The measured worst case for a `Write` in the whole corpus is 2.8 s. Five
/// leaves headroom for a slow filesystem without making the rail sluggish: a
/// person looking up from another pane does not notice three seconds against
/// five, and a false "waiting" is far more expensive than a late one - it is
/// the thing that teaches someone to stop believing the dot.
pub(crate) const GRACE: Duration = Duration::from_secs(5);

/// Past this, an open call stops being evidence of anything.
///
/// A call that never closes is far more likely to be a record this reader could
/// not account for than a person who has been staring at one prompt for half an
/// hour. The bound is what keeps a single unrecognised result shape - the kind
/// a CLI update can introduce - from turning into a permanent "waiting" on
/// every session at once: after this the file simply stops voting and the
/// process decides. A person who really did leave a prompt open that long gets
/// no dot, which is the cheap direction of the two.
pub(crate) const TRUST_HORIZON: Duration = Duration::from_secs(30 * 60);

/// How a named tool executes, for the names this build has measured.
///
/// **Add a name here only after measuring it.** Everything unrecognised is
/// [`ToolExecution::Opaque`], about which nothing is claimed - so a mistake of
/// omission costs a late dot, while a mistake of commission would put a lie in
/// the rail.
///
/// Measured (25 largest transcripts, 9 418 pairs): `Read` never exceeded 1.4 s,
/// `Write` 2.8 s, `Edit` 1.54 s at the 99th percentile with a single outlier
/// that was itself almost certainly a permission wait. `NotebookEdit` is here
/// on physics rather than measurement - it is a file write - and is the one
/// entry without numbers behind it. `Bash` is the only tool observed to spawn
/// anything.
pub(crate) fn execution_of(name: &str) -> ToolExecution {
    match name {
        "Read" | "Write" | "Edit" | "NotebookEdit" => ToolExecution::Instant,
        "Bash" => ToolExecution::Spawns,
        _ => ToolExecution::Opaque,
    }
}

/// What to show for a session, from the file and the process together.
///
/// # Why nothing is ever claimed without evidence
///
/// The two mistakes are not equal. Saying "working" about an agent that is
/// actually waiting costs a late dot; the user finds it on their next glance,
/// or the next poll corrects it. Saying "waiting" about an agent that is busy
/// puts a false claim in the rail, and a rail that cries wolf is worse than one
/// that is quiet - it is the failure that makes the whole feature worthless.
///
/// So every path that reports waiting is backed by something positive, and
/// every path that cannot be is "working".
///
/// # Why the agent's own process is an input
///
/// Both arms below reason about a **live** session: the file says a call is
/// open, and the process says whether anything is executing it. Neither
/// statement means anything once the agent is gone - the file stops being a
/// live account and becomes a frozen one, and no later record will ever close
/// the call. So [`AgentProcess::NotSeen`] silences the waiting answer outright
/// rather than being folded into [`Worker::Unknown`], which the `Instant` arm
/// accepts. See [`AgentProcess`] for the measurement that forced this apart.
pub(crate) fn classify(probe: &TranscriptProbe, worker: Worker, agent: AgentProcess) -> AgentState {
    if probe.errored {
        return AgentState::Errored;
    }
    if probe.incomplete {
        // The file's account of what is open is missing a piece, so it gets no
        // vote. Only the process does - and with nothing running, "the turn is
        // over" is the honest reading, not "someone is being waited on".
        return match worker {
            Worker::Absent => AgentState::Finished,
            Worker::Present | Worker::Unknown => AgentState::Thinking,
        };
    }
    // Past the horizon a call stops being evidence **of anything**, which is
    // what [`TRUST_HORIZON`]'s own doc says and what this build did not do: the
    // bound was applied inside the waiting predicate only, so an expired call
    // was refused a vote for "waiting" and kept its vote for "working" through
    // the emptiness check below.
    //
    // The difference is not academic. A surface whose file is frozen with one
    // unanswered call - a session closed over an open one, which the local Codex
    // corpus contains - showed a spinner that never went out. Unreachable for
    // Claude Code, whose file is always the live session's, and reachable for
    // Codex the moment its reader landed.
    //
    // A call with **no** usable age is not expired: nothing is known about it,
    // which is a different thing from being old. It stays, and the waiting
    // predicate refuses it on its own terms.
    // **The caller's baseline condition asks the raw set, and that difference is
    // deliberate.** `app::agent_state_pass` takes the worker baseline on
    // `probe.open_calls.is_empty()`, not on this function answering `Finished`,
    // so an expired-only probe goes quiet **without** a baseline being taken -
    // which is what stops a genuinely long-running command from being absorbed
    // into the resting shape and hidden as a worker. Making the two agree would
    // undo that, so they are asymmetric on purpose rather than by oversight.
    //
    // Its price, named because a cross-vendor pass pointed at it: a file that
    // holds an expired call **and** keeps being written to never reaches a raw
    // empty set, so it never takes a baseline and its worker stays
    // `Worker::Unknown` - which the `Spawns` arm refuses, so that surface cannot
    // report waiting.
    //
    // **Rare, and not narrow by construction** - the distinction was worth a
    // correction. The tail is bounded at 512 KB, so an expired record leaves the
    // window only after that much *subsequent* output: a chatty session clears
    // it in minutes, a mostly-idle one can hold it for hours. The escape is
    // throughput-dependent. What does bound it is the blast radius:
    // `worker_baselines` is keyed by surface, so a surface stuck this way costs
    // only itself, and a frozen file is `Finished` and quiet meanwhile.
    //
    // **An `Opaque` call is exempt while the agent is still there**, and the
    // asymmetry is the reason. The horizon exists to stop an unrecognised
    // record shape from turning into a permanent claim about a *person* -
    // which is a claim only the `Instant` and `Spawns` arms can make. An
    // `Opaque` call has no waiting vote at all, so ageing one out cannot
    // prevent the mistake this bound was written for, and can only cost the
    // single answer it is able to give. Found by a cross-vendor pass on the
    // background-agent reader: a subagent that runs past half an hour is the
    // ordinary case, not the pathological one, and its session went quiet
    // while it worked.
    //
    // `AgentProcess::Found` is what replaces the horizon here, and it is a
    // better bound than a clock: the thing the horizon really guards against
    // is a *frozen file* - a record left open by a session that no longer
    // exists - and the process answers that directly rather than by guessing
    // how long is too long.
    let fresh: Vec<&OpenCall> = probe
        .open_calls
        .iter()
        .filter(|call| {
            if call.execution == ToolExecution::Opaque {
                return agent == AgentProcess::Found;
            }
            call.age.is_none_or(|age| age < TRUST_HORIZON)
        })
        .collect();
    if fresh.is_empty() {
        // **No call open is not the same as no turn open**, and reading them as
        // the same thing is the commonest missing dot this rail has.
        //
        // An agent generating - thinking, or writing the answer that ends the
        // turn - has nothing in flight and appends nothing while it does it.
        // Every such gap read as `Finished` here, so a session visibly working
        // said `idle`: 41 seconds of it in this repository's own transcript on
        // 31 August, which is the window the report that prompted this was
        // screenshotted in.
        //
        // The file states the difference rather than leaving it to be inferred
        // - `stop_reason` is `tool_use` while the turn continues and `end_turn`
        // when it is over - so [`TranscriptProbe::open_turn`] is a reading, not
        // a guess.
        //
        // Two bounds, and both are the ones already used next door. The
        // **process** is the real one: a turn left open by a session that no
        // longer exists is a frozen record, and `AgentProcess` answers that
        // directly. [`TRUST_HORIZON`] is the second, and unlike an `Opaque`
        // call a turn has no legitimate reason to outlive it - a generation gap
        // is seconds to minutes, and anything genuinely long is a call, which
        // never reaches this branch. What the horizon catches here is the file
        // that stopped being the live one: a transcript the session moved off
        // (`/clear` mints a new one) would otherwise spin forever.
        if let Some(age) = probe.open_turn
            && age < TRUST_HORIZON
            && agent == AgentProcess::Found
        {
            return AgentState::Thinking;
        }
        // Nothing in flight. The turn is over as far as the file knows; the
        // rail's own idle sweep decides how long to keep saying so.
        //
        // The price of the filter above, named rather than discovered: a single
        // tool call that genuinely runs longer than half an hour reads as
        // finished and gets no dot. That is a **missing** signal about work, not
        // a false one about a person, and it is the direction `TRUST_HORIZON`
        // was written to choose.
        //
        // A cross-vendor pass argued the opposite - that `Finished` shown to a
        // person is itself a positive claim, "done", as false as a false
        // "waiting". It is not, and the reason is one file over:
        // `ThreadStatus::from_agent_state` maps `Finished` to **`Idle`**, and
        // `agents_sidebar::status_dot` draws `Idle` in `faint` at half opacity -
        // the same treatment a bare shell gets, documented there as "no turn to
        // be in". So the word this rule returns is `Finished` and the thing a
        // person sees is the resting dot: no claim, which is exactly the
        // "unknown" label that argument asked for. Written down here because the
        // mapping is invisible from this file, and that is what made the reading
        // available.
        return AgentState::Finished;
    }
    // **Any** hanging call, not just the oldest: a turn can put several tools
    // in flight at once, and the one being held for an answer is often not the
    // one that started first.
    // Nothing under the pane is running this session any more (or the chain is
    // one this build cannot read). The open call below is a record, not a
    // question being asked of anybody, so no arm gets a vote.
    let waiting = agent == AgentProcess::Found
        && fresh.iter().any(|call| {
            let Some(age) = call.age else {
                return false;
            };
            // Only the lower bound is left here. The upper one moved above, to
            // the filter, so the horizon has one home and means one thing.
            if age < GRACE {
                return false;
            }
            match call.execution {
                // A worker under the agent refutes the file's guess, and refutation
                // wins. An edit can hang because the user's own `PostToolUse` hook
                // is running a formatter over a monorepo, and the child process
                // says so plainly. The price is named rather than hidden: a turn
                // holding a long `Bash` beside a waiting `Edit` goes unreported
                // until the command ends. That is a late dot, and it is the mistake
                // we are willing to make.
                ToolExecution::Instant => worker != Worker::Present,
                // A command that has spawned nothing has not started, and a command
                // that has not started is one waiting for an answer. `Unknown` is
                // not evidence, so it does not count.
                ToolExecution::Spawns => worker == Worker::Absent,
                // A network call or a subagent spawns nothing and may legitimately
                // run for minutes. Nothing separates the two, so nothing is said.
                ToolExecution::Opaque => false,
            }
        });
    if waiting {
        return AgentState::WaitingForInput;
    }
    AgentState::Thinking
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, secs: u64) -> OpenCall {
        OpenCall {
            execution: execution_of(name),
            name: name.to_string(),
            age: Some(Duration::from_secs(secs)),
        }
    }

    fn sample(at: i64, dollars: f64) -> SpendSample {
        SpendSample { at, dollars }
    }

    #[test]
    fn nothing_observed_is_not_a_rate_of_zero() {
        // The distinction this module keeps everywhere: silence is not an
        // answer. A surface with no priced record may be spending steadily.
        assert_eq!(spend_rate(&[], 10_000, 600), None);
    }

    #[test]
    fn the_rate_divides_by_the_span_it_saw_rather_than_the_window() {
        // Two minutes of history in a ten-minute window is a rate over two
        // minutes. Dividing by ten would report a fifth of the truth for the
        // first eight minutes of every session.
        let now = 10_000;
        let samples = [sample(now - 120, 0.50), sample(now - 30, 0.50)];
        let rate = spend_rate(&samples, now, 600).expect("two minutes is enough to divide by");
        assert_eq!(rate.messages, 2);
        assert!((rate.over_minutes - 2.0).abs() < 1e-9);
        assert!((rate.dollars_per_minute - 0.50).abs() < 1e-9, "{rate:?}");
    }

    #[test]
    fn spending_older_than_the_window_is_not_in_the_rate() {
        let now = 10_000;
        let samples = [sample(now - 5_000, 100.0), sample(now - 300, 1.0)];
        let rate = spend_rate(&samples, now, 600).expect("rate");
        assert_eq!(rate.messages, 1, "the old one is out of the window");
        assert!((rate.over_minutes - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_span_too_short_to_divide_by_is_no_rate_at_all() {
        // One message thirty seconds ago says nothing about a trend, and
        // dividing by half a minute would double whatever it did say.
        let now = 10_000;
        assert_eq!(spend_rate(&[sample(now - 30, 1.0)], now, 600), None);
        // Exactly at the floor is enough.
        assert!(spend_rate(&[sample(now - 60, 1.0)], now, 600).is_some());
    }

    #[test]
    fn a_record_stamped_in_the_future_is_a_clock_that_moved_not_a_cost() {
        let now = 10_000;
        let samples = [sample(now + 500, 9.0), sample(now - 300, 1.0)];
        let rate = spend_rate(&samples, now, 600).expect("rate");
        assert_eq!(rate.messages, 1);
        assert!((rate.dollars_per_minute - 0.2).abs() < 1e-9, "{rate:?}");
    }

    fn probe(calls: Vec<OpenCall>) -> TranscriptProbe {
        TranscriptProbe {
            spend: Vec::new(),
            open_calls: calls,
            open_turn: None,
            errored: false,
            incomplete: false,
        }
    }

    /// The same fixture with a turn in flight, `secs` old.
    fn probe_mid_turn(calls: Vec<OpenCall>, secs: u64) -> TranscriptProbe {
        TranscriptProbe {
            spend: Vec::new(),
            open_turn: Some(Duration::from_secs(secs)),
            ..probe(calls)
        }
    }

    /// The gap this rail had all along: no call open, and the agent working.
    ///
    /// The file says so with `stop_reason`, which is why this is a reading and
    /// not a guess. Everything before it read the empty call set as the end of
    /// the turn.
    #[test]
    fn a_turn_in_flight_with_no_call_open_is_work() {
        assert_eq!(
            classify(
                &probe_mid_turn(vec![], 30),
                Worker::Absent,
                AgentProcess::Found
            ),
            AgentState::Thinking
        );
    }

    /// And it never becomes a claim about a person. A turn in flight is the
    /// model generating **or** a permission ask the file cannot see, and
    /// nothing here separates the two - so the answer is the cheap one.
    #[test]
    fn a_turn_in_flight_never_reports_a_person_waiting() {
        for worker in [Worker::Absent, Worker::Present, Worker::Unknown] {
            assert_eq!(
                classify(
                    &probe_mid_turn(vec![], 60 * 20),
                    worker,
                    AgentProcess::Found
                ),
                AgentState::Thinking
            );
        }
    }

    /// The process is the real bound. A turn left open by a session that is
    /// gone is a frozen record, and no later record will ever close it.
    #[test]
    fn a_turn_open_under_a_missing_agent_says_nothing() {
        assert_eq!(
            classify(
                &probe_mid_turn(vec![], 30),
                Worker::Absent,
                AgentProcess::NotSeen
            ),
            AgentState::Finished
        );
    }

    /// The horizon is the second bound, and unlike an `Opaque` call a turn has
    /// no legitimate reason to outlive it: a generation gap is seconds, and
    /// anything genuinely long is a call, which never reaches this branch.
    /// What it catches is the transcript a session has moved off - `/clear`
    /// mints a new one - which would otherwise spin for as long as the process
    /// lived.
    #[test]
    fn a_turn_older_than_the_horizon_stops_voting() {
        let stale = probe_mid_turn(vec![], TRUST_HORIZON.as_secs() + 1);
        assert_eq!(
            classify(&stale, Worker::Absent, AgentProcess::Found),
            AgentState::Finished
        );
    }

    /// It is consulted only where the call set is empty, so it cannot cover up
    /// the one answer that matters most.
    #[test]
    fn a_turn_in_flight_does_not_mask_a_person_being_waited_on() {
        let probe = probe_mid_turn(vec![call("Write", 30)], 30);
        assert_eq!(
            classify(&probe, Worker::Absent, AgentProcess::Found),
            AgentState::WaitingForInput
        );
    }

    /// A background agent outlives the horizon, because the horizon is not the
    /// bound that fits it.
    ///
    /// Reported by a cross-vendor pass, and the argument is the asymmetry: an
    /// `Opaque` call can only ever vote "working", so expiring one cannot
    /// prevent a false claim about a person and can only cost the single true
    /// one it had. A subagent past thirty minutes is ordinary.
    #[test]
    fn a_background_agent_keeps_voting_past_the_horizon() {
        let old = OpenCall {
            name: "background agent".to_string(),
            age: Some(TRUST_HORIZON + Duration::from_secs(60 * 60)),
            execution: ToolExecution::Opaque,
        };
        assert_eq!(
            classify(
                &probe(vec![old.clone()]),
                Worker::Absent,
                AgentProcess::Found
            ),
            AgentState::Thinking
        );
        // And it stops the moment the session it belongs to is gone: a frozen
        // file is what the horizon was really guarding against, and the process
        // answers that directly.
        assert_eq!(
            classify(&probe(vec![old]), Worker::Absent, AgentProcess::NotSeen),
            AgentState::Finished
        );
    }

    /// The exemption is for `Opaque` only - the two arms that can claim a
    /// person is waiting keep their clock.
    #[test]
    fn an_expired_instant_call_still_stops_voting() {
        let old = OpenCall {
            name: "Write".to_string(),
            age: Some(TRUST_HORIZON + Duration::from_secs(1)),
            execution: ToolExecution::Instant,
        };
        assert_eq!(
            classify(&probe(vec![old]), Worker::Absent, AgentProcess::Found),
            AgentState::Finished
        );
    }

    /// The case the whole module exists for, and the one the design chose to
    /// intercept: an edit that has not come back is a question.
    #[test]
    fn a_hanging_edit_is_a_question() {
        for worker in [Worker::Absent, Worker::Unknown] {
            assert_eq!(
                classify(&probe(vec![call("Edit", 9)]), worker, AgentProcess::Found),
                AgentState::WaitingForInput
            );
            assert_eq!(
                classify(&probe(vec![call("Write", 30)]), worker, AgentProcess::Found),
                AgentState::WaitingForInput
            );
        }
    }

    /// The direction the whole module is written to make impossible, measured
    /// live on 27 August before it was closed: a `Write` permission ask on
    /// screen, the agent SIGKILLed under it, its transcript frozen with the
    /// call still open - and the rail saying "waiting for you" about a session
    /// that no longer exists, for as long as `TRUST_HORIZON` allows.
    ///
    /// The guard that was supposed to stop this was the shim resolution
    /// refusing to name an agent, carried in as [`Worker::Unknown`]. It could
    /// not: only [`ToolExecution::Spawns`] demands [`Worker::Absent`], so the
    /// `Instant` arm read the missing refutation as permission to speak. The
    /// fact travels as [`AgentProcess`] now, and **no** arm gets a vote without
    /// it.
    #[test]
    fn a_dead_agent_never_reports_a_person_waiting() {
        for name in ["Edit", "Write", "Read", "NotebookEdit", "Bash"] {
            for worker in [Worker::Absent, Worker::Unknown, Worker::Present] {
                assert_eq!(
                    classify(&probe(vec![call(name, 120)]), worker, AgentProcess::NotSeen),
                    AgentState::Thinking,
                    "{name} / {worker:?}"
                );
            }
        }
    }

    /// And the price of that guard, stated rather than discovered: a live agent
    /// whose launch chain this build cannot read loses the answer entirely. A
    /// missing dot, never a false one - which is the trade the invariant asks
    /// for, but it is a trade and it belongs in a test.
    #[test]
    fn an_unreadable_chain_costs_the_answer_and_not_the_invariant() {
        let p = probe(vec![call("Edit", 30)]);
        assert_eq!(
            classify(&p, Worker::Absent, AgentProcess::Found),
            AgentState::WaitingForInput
        );
        assert_eq!(
            classify(&p, Worker::Absent, AgentProcess::NotSeen),
            AgentState::Thinking
        );
    }

    /// Inside the grace it is just a slow disk. The measured worst case for a
    /// write was 2.8 s, so three seconds must still read as work.
    #[test]
    fn an_edit_inside_the_grace_is_still_work() {
        assert_eq!(
            classify(
                &probe(vec![call("Write", 3)]),
                Worker::Absent,
                AgentProcess::Found
            ),
            AgentState::Thinking
        );
    }

    /// A worker refutes the file's guess and wins - even though it costs the
    /// parallel case. A formatter hook delaying an edit's result must not be
    /// reported as a person being kept waiting.
    #[test]
    fn a_worker_refutes_a_hanging_edit() {
        assert_eq!(
            classify(
                &probe(vec![call("Edit", 40)]),
                Worker::Present,
                AgentProcess::Found
            ),
            AgentState::Thinking
        );
    }

    /// A command decides on the worker, never on the clock: a fifth of them run
    /// past ten seconds legitimately.
    #[test]
    fn a_command_is_decided_by_the_worker_and_not_the_clock() {
        assert_eq!(
            classify(
                &probe(vec![call("Bash", 600)]),
                Worker::Present,
                AgentProcess::Found
            ),
            AgentState::Thinking
        );
        assert_eq!(
            classify(
                &probe(vec![call("Bash", 600)]),
                Worker::Absent,
                AgentProcess::Found
            ),
            AgentState::WaitingForInput
        );
        // Nothing was asked, so nothing is claimed.
        assert_eq!(
            classify(
                &probe(vec![call("Bash", 600)]),
                Worker::Unknown,
                AgentProcess::Found
            ),
            AgentState::Thinking
        );
    }

    /// The counterexample process liveness cannot answer: an MCP call runs
    /// inside the agent, so it spawns nothing whether it is working or waiting.
    /// Neither signal separates them, so neither gets to claim.
    ///
    /// The ages here are **inside** the trust horizon on purpose. They used to
    /// be an hour, and this test was passing partly for the wrong reason: past
    /// the horizon a call is now filtered out entirely, so an expired fixture
    /// would prove the horizon works rather than that the `Opaque` arm does.
    #[test]
    fn a_networked_tool_is_never_called_a_question() {
        for worker in [Worker::Absent, Worker::Present, Worker::Unknown] {
            assert_eq!(
                classify(
                    &probe(vec![call("mcp__spike__weather", 600)]),
                    worker,
                    AgentProcess::Found
                ),
                AgentState::Thinking
            );
            assert_eq!(
                classify(
                    &probe(vec![call("WebFetch", 600)]),
                    worker,
                    AgentProcess::Found
                ),
                AgentState::Thinking
            );
        }
    }

    /// A subagent runs for tens of seconds legitimately and writes nothing the
    /// parent transcript can see, so it is opaque like the rest.
    #[test]
    fn a_subagent_is_not_a_question() {
        for worker in [Worker::Absent, Worker::Present, Worker::Unknown] {
            assert_eq!(
                classify(&probe(vec![call("Agent", 25)]), worker, AgentProcess::Found),
                AgentState::Thinking
            );
        }
    }

    /// A tool name this build has never seen must be opaque, not instant. This
    /// is what makes the rule survive the next release of an agent CLI.
    #[test]
    fn an_unknown_tool_claims_nothing() {
        for name in ["Task", "mcp__server__tool", "SomeFutureTool", ""] {
            assert_eq!(execution_of(name), ToolExecution::Opaque, "{name}");
        }
        for name in ["Read", "Write", "Edit", "NotebookEdit"] {
            assert_eq!(execution_of(name), ToolExecution::Instant, "{name}");
        }
        assert_eq!(execution_of("Bash"), ToolExecution::Spawns);
    }

    /// A hanging edit is not hidden behind an older call, so long as nothing
    /// refutes it.
    #[test]
    fn a_hanging_edit_is_not_hidden_behind_an_older_call() {
        let p = probe(vec![call("Agent", 600), call("Edit", 40)]);
        assert_eq!(
            classify(&p, Worker::Unknown, AgentProcess::Found),
            AgentState::WaitingForInput
        );
    }

    /// A call with no usable timestamp is not evidence. Reading a missing stamp
    /// as zero would have made every call instantly older than any grace.
    #[test]
    fn a_call_with_no_age_is_not_evidence() {
        let p = probe(vec![OpenCall {
            name: "Write".to_string(),
            age: None,
            execution: ToolExecution::Instant,
        }]);
        assert_eq!(
            classify(&p, Worker::Absent, AgentProcess::Found),
            AgentState::Thinking
        );
    }

    /// A call open for half an hour is a record we lost, not a person still
    /// staring at a prompt - and that bound is what stops one unrecognised
    /// result shape from claiming "waiting" on every session forever.
    ///
    /// **This test asserted the opposite of its own name until 27 August.** It
    /// checked that such a call produced `Thinking`, which is a vote: the
    /// horizon was applied inside the waiting predicate only, so an expired call
    /// lost its vote for "waiting" and kept its vote for "working". A surface
    /// whose file froze with one unanswered call therefore span for ever - dead
    /// ground for Claude Code, whose file is always the live session's, and live
    /// ground for Codex the moment its reader landed.
    #[test]
    fn a_call_open_past_the_horizon_stops_voting() {
        for worker in [Worker::Absent, Worker::Present, Worker::Unknown] {
            assert_eq!(
                classify(
                    &probe(vec![call("Write", 31 * 60)]),
                    worker,
                    AgentProcess::Found
                ),
                AgentState::Finished,
                "an expired call is evidence of nothing, including of work \
                 ({worker:?})"
            );
        }
    }

    /// The horizon takes a call's vote away; it does not take the turn away.
    /// A fresh call standing beside an expired one still speaks for itself.
    #[test]
    fn an_expired_call_does_not_silence_a_fresh_one() {
        let p = probe(vec![call("Write", 31 * 60), call("Edit", 30)]);
        assert_eq!(
            classify(&p, Worker::Absent, AgentProcess::Found),
            AgentState::WaitingForInput
        );
    }

    /// The shape this whole change was made for: a session closed over an
    /// unanswered call, its file frozen, the surface restored on top of it. One
    /// such rollout sits in the local Codex corpus. The old rule span for ever;
    /// the horizon makes it quiet, which is the honest answer about a turn
    /// nobody can account for any more.
    #[test]
    fn a_file_frozen_with_an_ancient_call_goes_quiet_rather_than_spinning() {
        let p = probe(vec![OpenCall {
            name: "exec_command".to_string(),
            age: Some(Duration::from_secs(243_472)),
            execution: ToolExecution::Spawns,
        }]);
        for worker in [Worker::Absent, Worker::Present, Worker::Unknown] {
            assert_eq!(
                classify(&p, worker, AgentProcess::Found),
                AgentState::Finished,
                "{worker:?}"
            );
        }
    }

    /// A big `Read` result blows past the per-line cap, its record cannot be
    /// parsed, and the call it would have closed stays open. The rule must not
    /// turn that into a person being kept waiting.
    #[test]
    fn a_window_with_an_unreadable_record_makes_no_claim_from_the_file() {
        let mut p = probe(vec![call("Read", 3600)]);
        p.incomplete = true;
        assert_eq!(
            classify(&p, Worker::Absent, AgentProcess::Found),
            AgentState::Finished
        );
        assert_eq!(
            classify(&p, Worker::Present, AgentProcess::Found),
            AgentState::Thinking
        );
        assert_eq!(
            classify(&p, Worker::Unknown, AgentProcess::Found),
            AgentState::Thinking
        );
    }

    /// Nothing in flight is the ordinary end of a turn.
    #[test]
    fn no_open_call_means_the_turn_is_over() {
        assert_eq!(
            classify(&probe(vec![]), Worker::Absent, AgentProcess::Found),
            AgentState::Finished
        );
    }

    /// The transcript states failures rather than leaving them to be guessed,
    /// and a failure outranks everything else in flight.
    #[test]
    fn a_stated_failure_wins() {
        let mut p = probe(vec![call("Edit", 60)]);
        p.errored = true;
        assert_eq!(
            classify(&p, Worker::Present, AgentProcess::Found),
            AgentState::Errored
        );
    }
}
