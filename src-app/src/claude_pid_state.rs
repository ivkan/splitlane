//! What Claude Code says about itself, in the file it keeps for `claude ps`.
//!
//! The CLI maintains one JSON file per live process at
//! `<claude config dir>/sessions/<pid>.json`, and since 2.1.x it writes a
//! `status` into it: `idle`, `busy`, `waiting` (with a human-readable
//! `waitingFor`), plus `shell` for "the turn is over but a background
//! `local_bash` is still alive". Those are the rail's three answers, published
//! by the vendor, and the CLI's own TUI paints its indicator from the same
//! three words.
//!
//! # Why this is a better source than the rule next door
//!
//! [`crate::agent_state::classify`] answers the same question by inference: a
//! call left open in the transcript, plus whether the agent's process has
//! spawned a worker that was not there when the turn ended. It is careful, it
//! is measured, and it is still inference - it cannot see a `Bash` permission
//! ask at all on 2.1.246, because nothing reaches the transcript until after
//! the person answers. This file says `waiting` while that ask is on screen.
//!
//! It also sidesteps the long-lived-child failure entirely - a helper process
//! that lives as long as the agent makes "has the agent spawned a child" answer
//! yes for ever, so the rule never reports a person waiting. Here the process
//! question stops being asked, so a
//! stdio MCP server being a permanent child of the agent - which our own
//! `splitlane mcp install` creates - cannot silence anything.
//!
//! The rule stays. This is the first source for the one agent that publishes
//! it; every other agent, and this one whenever the file says nothing, still
//! goes through the rule.
//!
//! # Measured, 27 August 2026, CLI 2.1.247
//!
//! Live probe in a real pty:
//!
//! ```text
//!  2.14s  {pid, kind:"interactive", entrypoint:"cli"}   file created, no status yet
//!  2.44s  status="idle"
//!  8.50s  status="busy"                                 +0.3s after the prompt
//! 18.54s  status="waiting"  waitingFor="permission prompt"
//! ```
//!
//! Four facts from that probe are load-bearing here, and each one is a reason
//! for a specific refusal below:
//!
//! 1. **`status` is written only for a terminal `entrypoint`** (`cli`). A
//!    session started by an IDE extension (`claude-vscode`) creates the file
//!    with every identifying field and never writes a status into it. So a file
//!    without `status` is "not saying", not "idle".
//! 2. **`claude -p` writes no file at all.** Absence is not evidence.
//! 3. **An inherited `CLAUDE_CODE_CHILD_SESSION` suppresses the file too** -
//!    the agent decides it is a nested session and stays out of the registry.
//!    `splitlane_acp::INHERITED_AGENT_SESSION_ENV` strips that from every PTY, so
//!    a pane will not hit it; it is a second reason why a missing file must read
//!    as silence.
//! 4. **A dead process can leave its last status on disk for ever.** On 2.1.247
//!    nothing removed these files: the probe left one reading
//!    `status: "waiting"`, `waitingFor: "permission prompt"` behind a process
//!    that no longer existed. 2.1.278 removes its own file on an orderly exit
//!    (measured 19 September 2026: gone ~2 s after `SIGTERM`), but a process
//!    that is killed or crashes cannot clean up after itself, so a stale file
//!    is still a case this module has to survive.
//!    Reporting a stale status would be the one failure the detector must never commit -
//!    claiming a person is needed on no evidence: a dot saying a person is
//!    being kept waiting by a session that is gone. So every caller resolves
//!    the pid from a live process snapshot first, and this module never reads a
//!    pid it was not handed from one.
//!
//! # The window that guard leaves, stated rather than papered over
//!
//! A pid handed here was alive when the pass captured its process table, not
//! necessarily when this file is read - the pass snapshots once and then works
//! through its surfaces. An agent that dies inside that window has its orphaned
//! `waiting` read as current, for one tick.
//!
//! **A liveness call here would not close it**, it would only move it: the
//! process can die between that call and the read just as easily. What it would
//! do is promise a cover it does not give, which is the exact defect `ced26c0`
//! was written to undo. So the window is named instead, with its bounds:
//!
//! - it needs the death to land inside a few milliseconds of one pass;
//! - it lasts exactly one tick. The next pass finds nothing under the PTY,
//!   `agent_under_pty` answers `None`, this source is never asked, and the rule
//!   takes over with [`crate::agent_state::AgentProcess::NotSeen`] - which
//!   silences the waiting answer outright;
//! - and it is the pass's shape, not this source's: the rule reads a transcript
//!   frozen by the same death against the same snapshot.
//!
//! A guard that closed it would have to make the reading and the liveness proof
//! one operation. Nothing on three platforms offers that, so the honest thing is
//! a bounded wrong answer for two seconds rather than a check that reads as
//! certainty.
//!
//! # Why the session id is the guard and the clock is not
//!
//! A pid can be recycled, and the file for the old owner stays. The obvious
//! guard is a time comparison - the file carries `procStart` and `startedAt` -
//! but that means parsing a locale-formatted string or reconciling two clocks
//! against a platform-specific start token, and getting it wrong fails open.
//!
//! The file also carries `waitingFor` (measured: the free string
//! `"permission prompt"`). Nothing reads it yet, so nothing exposes it - a
//! tooltip on the rail's waiting dot is the obvious home, and it is one
//! accessor away when something asks for it.
//!
//! There is an exact key instead. Splitlane *chose* the session uuid for this
//! surface and passed it to the CLI as `--session-id` / `--resume`
//! ([`crate::project::Thread::session_id`]), and the file reports the
//! `sessionId` the process is actually running. Equal means this file is this
//! surface's, whatever the pids have been doing. When the surface has no forced
//! id there is nothing to check against, and this module says nothing rather
//! than guessing.

use std::path::{Path, PathBuf};

use crate::ai_types::AgentState;

/// A bounded read: this file is a few hundred bytes and has no reason to grow.
/// A pathological one is not parsed at all.
const MAX_PID_FILE_BYTES: u64 = 64 * 1024;

/// The status vocabulary the CLI writes, mapped to what the rail says.
///
/// `shell` resolves like `idle` on purpose: it means the turn is over while a
/// background `local_bash` task is still alive. Nimbalyst shipped it as
/// "running" once and every session stayed on "Thinking" until the process
/// exited - the same shape as our own failure where a long-lived child process
/// kept the session reading as working for ever, from the other direction.
fn state_from_status(status: &str) -> Option<AgentState> {
    match status {
        "busy" => Some(AgentState::Thinking),
        "waiting" => Some(AgentState::WaitingForInput),
        "idle" | "shell" => Some(AgentState::Finished),
        // Deliberately not a catch-all to `Finished`. An unrecognised word is a
        // CLI that has moved on, and the honest answer is silence - the caller
        // falls through to the rule, which is what it did before this module
        // existed. `shell` itself is the cautionary tale: it was added in
        // 2.1.141 and read as "unparseable" by a reader that defaulted the
        // wrong way.
        _ => None,
    }
}

/// Where Claude Code keeps its configuration.
///
/// `CLAUDE_CONFIG_DIR` is honoured because the CLI honours it, and a user who
/// sets it has no `~/.claude` at all - every lookup rooted there silently finds
/// nothing, which reads exactly like "no sessions".
pub fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    Some(dirs::home_dir()?.join(".claude"))
}

/// The file `claude ps` reads for one process.
/// `None` for pid 0, which names no process on any of the three platforms.
/// [`crate::process_tree::ProcessSnapshot::identity_of`] and `descendants_of`
/// refuse it the same way and for the same reason: a total function that
/// happily composes a path for a pid that cannot exist is one refactor away
/// from reading `sessions/0.json` and believing it.
pub fn pid_file_path(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    Some(
        claude_config_dir()?
            .join("sessions")
            .join(format!("{pid}.json")),
    )
}

/// What the agent at `agent_pid` says it is doing, or `None` for "not saying".
///
/// `expect_session` is the surface's forced session uuid. When it is `Some` the
/// file's own `sessionId` has to match it; when it is `None` this returns
/// `None` without reading anything, because there is then no way to tell this
/// surface's file from a recycled pid's.
///
/// **`agent_pid` must come from a live process snapshot taken in the same
/// pass.** This module does no liveness check of its own - see fact 4 above.
pub fn state_for(agent_pid: u32, expect_session: Option<&str>) -> Option<AgentState> {
    let expect_session = expect_session?;
    let path = pid_file_path(agent_pid)?;
    let raw = read_bounded(&path)?;
    state_from_pid_file(&raw, expect_session)
}

/// The session the process at `agent_pid` is **actually running**, when it says
/// so and says it about `cwd`.
///
/// # The thing this exists for: `/clear` mints a new session
///
/// Splitlane names a session on the command line and then assumes it keeps that
/// name for as long as the pane is open. It does not. `/clear` starts a fresh
/// conversation with a **new uuid and a new transcript**, and the old file is
/// frozen where it stood - measured in the local corpus on 31 August, where
/// five of five transcripts containing a `/clear` carry it as record index 4 of
/// a file that begins there.
///
/// Everything Splitlane knows about that surface then points at the dead file at
/// once: the detector reads a transcript that will never grow again and reports
/// `idle` for ever, [`state_for`] refuses the status file because the session
/// ids no longer match, and "copy the last answer" hands back an answer from
/// before the clear. All three were found in use inside two days, and
/// all three are this.
///
/// # Why the file may be believed about this
///
/// `agent_pid` is resolved from a live process snapshot **under this pane's own
/// PTY child**, so the process is this surface's agent and not a guess. What is
/// left is a stale file for a recycled pid, and `cwd` is the guard: the file has
/// to say it is running in the directory this surface is anchored to. The caller
/// then checks that the named session has a transcript in that same project, so
/// the id, the directory and the disk all have to agree before anything moves.
///
/// This is deliberately narrower than [`state_for`]'s guard rather than a
/// weakening of it. That one compares against an id Splitlane chose and can
/// therefore be exact; this one exists precisely because that id is the thing
/// that has gone stale, so it is answered from the two facts left standing.
pub fn session_for(agent_pid: u32, cwd: &str) -> Option<String> {
    let path = pid_file_path(agent_pid)?;
    let raw = read_bounded(&path)?;
    session_from_pid_file(&raw, cwd)
}

/// The parse for [`session_for`], split out so the guard is testable without a
/// filesystem. Every refusal is `None`, which leaves the surface exactly where
/// it was.
fn session_from_pid_file(raw: &str, cwd: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = value.as_object()?;
    // An empty cwd is refused before the comparison, for the same reason
    // `state_from_pid_file` refuses an empty session id: a match that both
    // halves can satisfy by being absent is not a guard.
    if cwd.is_empty() {
        return None;
    }
    if !same_directory(obj.get("cwd").and_then(|v| v.as_str())?, cwd) {
        return None;
    }
    let session = obj.get("sessionId").and_then(|v| v.as_str())?;
    crate::agent_sessions::is_valid_session_id(session).then(|| session.to_string())
}

/// Whether two directory strings name the same directory, to the extent this
/// can be told without touching the disk.
///
/// A trailing separator only: `/a/b` and `/a/b/` are the same project, which is
/// the normalisation `claude_sessions::project_dir_for_cwd` already makes when
/// it composes the slug. Nothing more is attempted - a path spelled differently
/// through a symlink simply fails the guard and nothing is adopted, which is the
/// direction every refusal in this module goes.
fn same_directory(a: &str, b: &str) -> bool {
    fn trimmed(path: &str) -> &str {
        let cut = path.trim_end_matches(['/', '\\']);
        if cut.is_empty() { path } else { cut }
    }
    trimmed(a) == trimmed(b)
}

/// Read at most [`MAX_PID_FILE_BYTES`]. `None` for missing, unreadable or
/// absurdly large - all of which are silence.
fn read_bounded(path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_PID_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The parse, split out so the whole contract is testable without a filesystem.
///
/// Every refusal here returns `None`, and `None` means the caller uses the rule
/// instead. Nothing in this function can produce a wrong answer; it can only
/// decline to produce one.
fn state_from_pid_file(raw: &str, expect_session: &str) -> Option<AgentState> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = value.as_object()?;

    // The guard. A file that names a different session is somebody else's -
    // most likely a recycled pid whose predecessor's file was never cleaned up.
    // A file with no `sessionId` at all is not trusted either: it would let
    // exactly the case the guard exists for through.
    //
    // An **empty** expected id is refused before the comparison, because `"" ==
    // ""` is a match that proves nothing: it would pair any surface whose
    // recorded id got lost (a hand-edited or truncated `session.json`) with any
    // file that happens to carry an empty one. A guard that can be satisfied by
    // the absence of both halves is not a guard.
    if expect_session.is_empty() {
        return None;
    }
    if obj.get("sessionId").and_then(|v| v.as_str()) != Some(expect_session) {
        return None;
    }

    let status = obj.get("status").and_then(|v| v.as_str())?;
    state_from_status(status.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "cd55b6a2-b16f-4a54-b9dd-235b3747d2ed";

    /// The three shapes measured live on 2.1.247, verbatim in schema.
    fn file(status: &str) -> String {
        format!(
            r#"{{"pid":71306,"sessionId":"{SID}","cwd":"/tmp/wd","startedAt":1787826645649,
            "procStart":"Thu Aug 27 10:30:43 2026","version":"2.1.247","kind":"interactive",
            "entrypoint":"cli","status":"{status}","updatedAt":1787826661568,
            "statusUpdatedAt":1787826661568}}"#
        )
    }

    /// The `/clear` case, which is what the whole adoption path is for: the
    /// process is running a session Splitlane never chose.
    #[test]
    fn the_session_the_process_reports_is_read_back() {
        assert_eq!(
            session_from_pid_file(&file("busy"), "/tmp/wd").as_deref(),
            Some(SID)
        );
    }

    /// A trailing separator is the same directory. `project_dir_for_cwd` makes
    /// the same normalisation when it composes the slug, so a guard that did
    /// not would refuse exactly the surfaces whose transcripts it can find.
    #[test]
    fn a_trailing_separator_is_the_same_directory() {
        assert_eq!(
            session_from_pid_file(&file("busy"), "/tmp/wd/").as_deref(),
            Some(SID)
        );
    }

    /// The guard. A stale file left by a recycled pid is refused on the one
    /// fact it cannot fake: which directory the process is running in.
    #[test]
    fn a_file_about_another_directory_is_refused() {
        assert_eq!(session_from_pid_file(&file("busy"), "/tmp/other"), None);
    }

    /// A match both halves can satisfy by being absent is not a guard - the
    /// same refusal `state_from_pid_file` makes for an empty session id.
    #[test]
    fn an_empty_directory_is_refused_before_the_comparison() {
        let raw = r#"{"pid":1,"sessionId":"x","cwd":""}"#;
        assert_eq!(session_from_pid_file(raw, ""), None);
    }

    /// Nothing that is not a session id is adopted. The value goes on to
    /// compose a path, and `is_valid_session_id` is the one place that says
    /// what may.
    #[test]
    fn a_session_id_that_is_not_one_is_refused() {
        let raw = r#"{"pid":1,"sessionId":"../../etc/passwd","cwd":"/tmp/wd"}"#;
        assert_eq!(session_from_pid_file(raw, "/tmp/wd"), None);
    }

    /// A file with no session id at all says nothing, rather than reading as
    /// "no session".
    #[test]
    fn silence_about_the_session_is_not_an_answer() {
        let raw = r#"{"pid":1,"cwd":"/tmp/wd","status":"busy"}"#;
        assert_eq!(session_from_pid_file(raw, "/tmp/wd"), None);
    }

    #[test]
    fn the_three_words_map_to_the_three_answers() {
        assert_eq!(
            state_from_pid_file(&file("busy"), SID),
            Some(AgentState::Thinking)
        );
        assert_eq!(
            state_from_pid_file(&file("waiting"), SID),
            Some(AgentState::WaitingForInput)
        );
        assert_eq!(
            state_from_pid_file(&file("idle"), SID),
            Some(AgentState::Finished)
        );
    }

    /// `shell` is "turn over, background task alive" - it resolves like idle.
    /// Reading it as work is how nimbalyst pinned every session to "Thinking"
    /// for the life of the process.
    #[test]
    fn shell_is_not_work() {
        assert_eq!(
            state_from_pid_file(&file("shell"), SID),
            Some(AgentState::Finished)
        );
    }

    /// A word this build does not know is silence, not a default. The caller
    /// falls back to the rule, which is exactly where it was before.
    #[test]
    fn an_unknown_word_says_nothing() {
        assert_eq!(state_from_pid_file(&file("parked"), SID), None);
        assert_eq!(state_from_pid_file(&file(""), SID), None);
    }

    /// The measured VS Code shape: every identifying field, no `status`.
    /// It must not read as `Finished` - that would claim a stopped agent.
    #[test]
    fn a_file_without_a_status_says_nothing() {
        let vscode = format!(
            r#"{{"pid":20378,"sessionId":"{SID}","cwd":"/x","kind":"interactive",
            "entrypoint":"claude-vscode"}}"#
        );
        assert_eq!(state_from_pid_file(&vscode, SID), None);
    }

    /// The recycled-pid case, which is the whole reason the guard exists: a
    /// dead agent's file survives for ever, and the probe left one reading
    /// `waiting` behind a process that no longer existed.
    #[test]
    fn a_file_naming_another_session_is_refused() {
        assert_eq!(
            state_from_pid_file(&file("waiting"), "11111111-2222-3333-4444-555555555555"),
            None
        );
    }

    /// No `sessionId` means the guard cannot be applied, and an unguarded read
    /// is precisely what it is here to prevent.
    #[test]
    fn a_file_without_a_session_id_is_refused() {
        let anon = r#"{"pid":1,"status":"waiting"}"#;
        assert_eq!(state_from_pid_file(anon, SID), None);
    }

    /// `""` on both sides is a match that proves nothing, and a guard that the
    /// absence of both halves satisfies is not a guard.
    #[test]
    fn an_empty_session_id_never_matches() {
        let empty = r#"{"sessionId":"","status":"waiting"}"#;
        assert_eq!(state_from_pid_file(empty, ""), None);
        assert_eq!(state_from_pid_file(empty, SID), None);
        assert_eq!(state_from_pid_file(&file("waiting"), ""), None);
        assert_eq!(state_for(1234, Some("")), None);
    }

    /// pid 0 names no process anywhere, so it composes no path - the same
    /// refusal `process_tree` makes, for the same reason.
    #[test]
    fn pid_zero_composes_no_path() {
        assert_eq!(pid_file_path(0), None);
        assert_eq!(state_for(0, Some(SID)), None);
        assert!(pid_file_path(1).is_some());
    }

    #[test]
    fn malformed_json_says_nothing() {
        assert_eq!(state_from_pid_file("{not json", SID), None);
        assert_eq!(state_from_pid_file("[]", SID), None);
        assert_eq!(state_from_pid_file("", SID), None);
    }

    /// A surface with no forced session id has nothing to match against, so it
    /// gets no answer from here at all - rather than an unguarded one.
    #[test]
    fn a_surface_without_a_forced_id_is_never_answered() {
        assert_eq!(state_for(1, None), None);
    }

    /// `CLAUDE_CONFIG_DIR` is the CLI's own override; a user who sets it has no
    /// `~/.claude`, and a reader rooted there finds nothing while looking
    /// exactly like "no sessions".
    #[test]
    fn the_config_dir_honours_the_cli_override() {
        // Read-only assertion about shape: the path always ends in
        // `sessions/<pid>.json` under whatever root is resolved.
        let p = pid_file_path(4242).expect("a home or an override exists in test envs");
        assert!(
            p.ends_with(std::path::Path::new("sessions/4242.json")),
            "{p:?}"
        );
    }
}
