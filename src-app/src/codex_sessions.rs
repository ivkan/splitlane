//! Codex CLI session discovery - reads the on-disk transcript store at
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<TS>-<uuid>.jsonl` and produces
//! unified [`SessionMeta`](crate::agent_sessions::SessionMeta) entries
//! for the sessions popover.
//!
//! Format reference: PR openai/codex#3380 (RolloutItem envelope) and
//! community discussion #3827. The first line of every rollout file is a
//! `type:"session_meta"` envelope with `payload.id`, `payload.cwd`,
//! `payload.timestamp`, `payload.thread_source` and `payload.git.branch`.
//! Codex doesn't emit an `ai-title`-equivalent record, so the title falls
//! back to the first human-authored message (see [`user_text_from_record`]
//! for the three record shapes that carry one).
//!
//! All filesystem work happens off the GPUI main thread - call
//! [`read_sessions_for_cwd`] from inside `smol::unblock`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::agent_sessions::{AssistantUsage, SessionAgent, SessionMeta, clean_session_label};

/// Maximum number of leading lines to scan for the first user message.
/// In practice this lands within the first ~10 lines: on a 0.149.1 rollout
/// measured here the first `role:"user"` record is line 6 (an injected
/// `<environment_context>`) and the `UserMessage` marker is line 10. The cap
/// is generous so unusual prelude sequences still produce a label.
const TITLE_SCAN_LIMIT: usize = 256;

/// Byte budget for the same scan, and the binding constraint in practice:
/// the prelude lines are large (tens of KB each), and the line cap alone
/// would allow 256 x [`MAX_LINE_BYTES`] = 16 MiB per file, on every sidebar
/// refresh, for every rollout on disk.
const TITLE_SCAN_BYTES: u64 = 1024 * 1024;

/// Dedicated cap for line 1. [`MAX_LINE_BYTES`] (64 KiB) is the right bound
/// for body records but not for `session_meta`, which embeds the base
/// instructions and the tool list: 18-22 KB on this machine's rollouts,
/// 49.6 KB on upstream's, and it grows with every tool Codex ships.
/// Overrunning the cap costs the whole file (id and cwd live nowhere else),
/// so this one line gets its own, larger bound.
const SESSION_META_MAX_BYTES: u64 = 1024 * 1024;

/// Prefixes of the synthetic `role:"user"` records Codex injects around a real
/// prompt: repo instructions, skill bodies, plugin catalogs, environment
/// context. They are indistinguishable from human input at the record level,
/// so the title scan filters them by prefix.
const SYNTHETIC_USER_PREFIXES: [&str; 8] = [
    "# AGENTS.md",
    "<app-context",
    "<environment_context",
    "<permissions",
    "<recommended_plugins",
    "<skill>",
    "<system",
    "<user_instructions",
];

/// Deeper line cap for the attribution scan, which walks the
/// whole rollout to capture the model (`turn_context.payload.model`) and the
/// last cumulative `token_count` usage event. Bounded, and run ONLY on the
/// attribution path (the diff column load), never on the popover title scan.
const MODEL_USAGE_SCAN_LIMIT: usize = 20_000;

// Per-line JSONL read cap, centralized (see `crate::limits`).
use crate::limits::MAX_LINE_BYTES;

/// Cap rendered first-user-message labels at this character count.
const LABEL_MAX_CHARS: usize = 80;

/// One of Codex's own rate-limit windows, as its `token_count` event states it.
///
/// Mirrors `RateLimitWindow` in `codex-rs/protocol/src/protocol.rs` field for
/// field, deliberately: Codex is open source, so this shape is read from the
/// definition rather than inferred from samples, and keeping the names
/// identical is what lets the next reader check us against it.
///
/// **Re-checked against upstream on 9 September**, because the whole account
/// path turns on one word here and no file on the machine could settle it: the
/// reset is `resets_at`, and upstream's own comment says "Unix timestamp
/// (seconds since epoch) when the window resets" - absolute, not a duration.
/// Read as a duration it would have been a moment in 1970, and every window
/// would have looked long since reset.
///
/// `window_minutes` has no counterpart in the Claude path, where the two
/// windows are fixed and the words "5-hour" and "weekly" are ours. Here the
/// server states the length, so a label has to be derived from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CodexRateLimitWindow {
    /// Per cent of the window consumed, 0-100.
    pub(crate) used_percent: f64,
    /// Rolling window length in minutes, when stated.
    pub(crate) window_minutes: Option<i64>,
    /// Unix seconds at which the window resets, when stated.
    pub(crate) resets_at: Option<i64>,
}

/// What one `token_count` event says about the account's limits.
///
/// Mirrors `RateLimitSnapshot` upstream, minus the spend-control members
/// (`credits`, `individual_limit`, `spend_control_reached`): those answer a
/// different question - money left on a budget rather than share of a window
/// consumed - and folding them in here would be the same category mistake as
/// putting a spend figure in a footer that speaks in windows. `normal_model_slug`
/// is left out for the same reason it exists there: it is metadata about a
/// quota alias, not a limit.
///
/// Two members are enums upstream (`plan_type`, `rate_limit_reached_type`) and
/// strings here. A unit variant serialises to a JSON string, so the read is
/// right for every shape either has taken; both are stored and neither feeds
/// anything yet, so a variant that ever serialised as an object would read as
/// absent rather than wrong.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodexRateLimits {
    pub(crate) limit_id: Option<String>,
    pub(crate) limit_name: Option<String>,
    pub(crate) primary: Option<CodexRateLimitWindow>,
    pub(crate) secondary: Option<CodexRateLimitWindow>,
    pub(crate) plan_type: Option<String>,
    /// Which limit the account actually ran into, when it has.
    pub(crate) rate_limit_reached_type: Option<String>,
}

/// Read one window. `used_percent` is the only member upstream declares
/// non-optional, so a window without it is malformed rather than empty.
fn parse_rate_limit_window(value: &serde_json::Value) -> Option<CodexRateLimitWindow> {
    Some(CodexRateLimitWindow {
        used_percent: value.get("used_percent").and_then(|v| v.as_f64())?,
        window_minutes: value.get("window_minutes").and_then(|v| v.as_i64()),
        resets_at: value.get("resets_at").and_then(|v| v.as_i64()),
    })
}

/// Read the `rate_limits` object that sits beside `info` in a `token_count`
/// event.
///
/// **A snapshot carrying neither window is "nothing said", not "zero used".**
/// Codex writes the object with every member null when the account has no plan
/// window at all - which is what an API key has, and what every file on the
/// machine this was measured on looks like (the object is present in 12 of 18
/// rollouts there, and null in all of them). Treating that as a reading would
/// put a row on screen asserting 0% about a limit that does not exist, which is
/// the same failure the rail's footer already refuses when it says
/// "unavailable" rather than disappearing.
fn parse_rate_limits(value: &serde_json::Value) -> Option<CodexRateLimits> {
    let str_field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let snapshot = CodexRateLimits {
        limit_id: str_field("limit_id"),
        limit_name: str_field("limit_name"),
        primary: value.get("primary").and_then(parse_rate_limit_window),
        secondary: value.get("secondary").and_then(parse_rate_limit_window),
        plan_type: str_field("plan_type"),
        rate_limit_reached_type: str_field("rate_limit_reached_type"),
    };
    (snapshot.primary.is_some() || snapshot.secondary.is_some()).then_some(snapshot)
}

/// What Codex's newest statement about the account's limits actually was.
///
/// Two answers, and the difference between them is the whole reason this is not
/// an `Option`. **A login billed by an API key has no plan window at all**, and
/// Codex says so by writing the object with every member null - which it does
/// all day, on every `token_count` event. That is a fact about the account, and
/// a row that states it (`Codex - no plan limit`) is the difference between "we
/// do not support this vendor" and "there is nothing to show"; collapsing it
/// into "nothing found" throws that away.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CodexAccountLimits {
    /// Windows, as the newest event that named any stated them.
    Windows {
        limits: CodexRateLimits,
        /// Unix seconds: when **Codex** wrote that line, which is not when we
        /// read it.
        observed_at: i64,
    },
    /// The object is written and holds no window: this login has no plan
    /// window.
    NoPlanWindow { observed_at: i64 },
}

/// How many recent rollouts to look inside before giving up. The newest file is
/// almost always the answer; the rest are for the case where the last session
/// was one prompt long and ended before any `token_count` event.
const ACCOUNT_LIMITS_FILES: usize = 4;

/// How much of a rollout's tail to read. The limits ride on `token_count`
/// events, which are among the last lines of any turn, so the end of the file
/// is where they are - and reading the whole rollout to find the last one would
/// be megabytes of JSON per poll for a number that moves once a turn.
const ACCOUNT_LIMITS_TAIL_BYTES: u64 = 256 * 1024;

/// Codex's newest statement about the **account's** limits.
///
/// **Per account, not per session**, which is the rule the note keeps and the
/// reason this does not go anywhere near `read_sessions_for_cwd`: a rate limit
/// belongs to the login, so asking it once per project would be the same
/// question asked N times and answered N ways. One read, newest rollout first,
/// whatever directory it was opened in.
///
/// **Blocking I/O** - call from inside `smol::unblock`, never on the GPUI
/// thread.
/// `horizon` bounds the walk to rollouts written inside it, which is the cheap
/// half of the freshness rule: the expensive half - whether the *statement* is
/// old enough that the row should go - is the caller's, because it is about
/// what is drawn rather than about what is on disk.
pub(crate) fn read_account_rate_limits(horizon: Duration) -> Option<CodexAccountLimits> {
    let root = sessions_root()?;
    let horizon = SystemTime::now().checked_sub(horizon);
    let mut recent: Vec<(SystemTime, PathBuf)> = Vec::new();
    walk_jsonl_files(&root, &mut |path| {
        let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) else {
            return;
        };
        if horizon.is_some_and(|horizon| modified < horizon) {
            return;
        }
        recent.push((modified, path.to_path_buf()));
    });
    // Newest first.
    recent.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

    // **The newest rollout that said anything wins, whatever it said.** An
    // earlier draft preferred windows over an emptier newer statement, on the
    // reasoning that the all-null object is what Codex writes when it has
    // nothing to say. That is true *inside* one session, where the nulls are
    // noise between two real readings - and false across sessions, because
    // `~/.codex/sessions` is one directory shared by every login this machine
    // has used. A person who moved from a plan to an API key would have been
    // shown yesterday's plan windows for as long as the horizon let them
    // stand: a limit asserted about an account that has none, which is the one
    // claim this row exists to avoid. Within a file the older rule still holds;
    // see `last_rate_limits_in_tail`.
    recent
        .iter()
        .take(ACCOUNT_LIMITS_FILES)
        .find_map(|(_, path)| last_rate_limits_in_tail(path))
}

/// The last `token_count` in this file that said anything about limits.
fn last_rate_limits_in_tail(path: &Path) -> Option<CodexAccountLimits> {
    let mut file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let from = length.saturating_sub(ACCOUNT_LIMITS_TAIL_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::with_capacity(length.saturating_sub(from) as usize);
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);

    let mut lines: Vec<&str> = tail.lines().collect();
    // The first line of a mid-file read is half a line.
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    let mut no_plan: Option<CodexAccountLimits> = None;
    for line in lines.iter().rev() {
        if !line.contains("token_count") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim_end()) else {
            continue;
        };
        let Some(rate_limits) = value
            .get("payload")
            .filter(|payload| payload.get("type").and_then(|t| t.as_str()) == Some("token_count"))
            .and_then(|payload| payload.get("rate_limits"))
            .filter(|rate_limits| rate_limits.is_object())
        else {
            continue;
        };
        // **A line we cannot date is a line we cannot use, and the file cannot
        // date it for us**: a rollout's mtime is when its *last* line was
        // written, so borrowing it would make this statement look newer than it
        // is - the wrong direction for an age, which must err old. So the line
        // is skipped and the scan goes on to the one before it, which is very
        // likely dated: every rollout on the machine this was measured on
        // carries an RFC 3339 `timestamp` on every line.
        let Some(observed_at) = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|when| when.timestamp())
        else {
            continue;
        };
        match parse_rate_limits(rate_limits) {
            Some(limits) => {
                return Some(CodexAccountLimits::Windows {
                    limits,
                    observed_at,
                });
            }
            // The object with nothing in it. Keep the newest one and go on
            // looking: Codex writes it beside real readings too, and "the last
            // one that says anything wins" is the rule the usage scan already
            // follows.
            None => {
                no_plan = no_plan.or(Some(CodexAccountLimits::NoPlanWindow { observed_at }));
            }
        }
    }
    no_plan
}

/// Compute the absolute path of `~/.codex/sessions/`. Returns `None` when
/// `dirs::home_dir()` fails.
pub fn sessions_root() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".codex").join("sessions"))
}

/// Read all Codex CLI sessions whose recorded `cwd` matches the given
/// directory. Returns sessions sorted by timestamp descending (most
/// recent first).
///
/// **Blocking I/O** - call from inside `smol::unblock` or
/// `cx.background_executor`. Codex's flat date-bucketed layout
/// (`YYYY/MM/DD`) means we must scan every rollout file and read the
/// first line to filter by `cwd`. For the typical user (≤ 200 sessions)
/// this is comfortably under 100 ms; cap heavy users via the
/// per-file fast bail-out (we stop after the session_meta line if cwd
/// doesn't match).
pub fn read_sessions_for_cwd(cwd: &str) -> Vec<SessionMeta> {
    read_sessions_for_cwd_with_omitted(cwd).0
}

/// Like [`read_sessions_for_cwd`], but also reports how many older matching
/// sessions were omitted by the sidebar retention cap.
pub fn read_sessions_for_cwd_with_omitted(cwd: &str) -> (Vec<SessionMeta>, usize) {
    read_sessions_for_cwd_inner(
        cwd,
        false,
        Some(crate::agent_sessions::SIDEBAR_SESSION_RETAINED_PER_SOURCE),
    )
}

/// Like [`read_sessions_for_cwd`] but the retained
/// attribution candidates are scanned deeper to populate `model`
/// (`turn_context`) + cumulative `usage` (last `token_count` event).
/// **Blocking I/O** - call from inside `smol::unblock`.
pub fn read_sessions_with_usage_for_attribution(cwd: &str, branch: &str) -> Vec<SessionMeta> {
    let Some(root) = sessions_root() else {
        return Vec::new();
    };

    let mut candidates: Vec<(SessionMeta, PathBuf)> = Vec::new();
    walk_jsonl_files(&root, &mut |path| {
        if let Some(meta) = read_session_meta_inner(path, false, Some(cwd)) {
            crate::agent_sessions::push_ranked_attribution(
                &mut candidates,
                meta,
                path.to_path_buf(),
                branch,
                crate::agent_sessions::DIFF_ATTRIBUTION_MATCH_CAP,
            );
        }
    });

    let enriched: Vec<SessionMeta> = candidates
        .into_iter()
        .map(|(fallback, path)| {
            // The deep re-read can fail where the cheap head scan succeeded (an
            // I/O error, a body that outruns the usage budget); keep the
            // already-matched head result rather than dropping the column.
            read_session_meta_inner(&path, true, Some(cwd)).unwrap_or(fallback)
        })
        .collect();
    crate::agent_sessions::match_sessions_to_column(enriched, cwd, branch)
}

fn read_sessions_for_cwd_inner(
    cwd: &str,
    scan_usage: bool,
    cap: Option<usize>,
) -> (Vec<SessionMeta>, usize) {
    let Some(root) = sessions_root() else {
        return (Vec::new(), 0);
    };

    let cache_mtime = (!scan_usage && cap.is_some())
        .then(|| jsonl_tree_mtime(&root))
        .flatten();
    if let Some(cache_mtime) = cache_mtime
        && let Some(cached) =
            crate::agent_sessions::cache::lookup_with_mtime(SessionAgent::Codex, cwd, cache_mtime)
    {
        return cached;
    }

    let result = match cap {
        Some(cap) => {
            let mut collector = crate::agent_sessions::RecentSessionCollector::new(cap);
            walk_jsonl_files(&root, &mut |path| {
                if let Some(meta) = read_session_meta_inner(path, scan_usage, Some(cwd)) {
                    collector.push(meta);
                }
            });
            collector.finish()
        }
        None => {
            let mut all = Vec::new();
            walk_jsonl_files(&root, &mut |path| {
                if let Some(meta) = read_session_meta_inner(path, scan_usage, Some(cwd)) {
                    all.push(meta);
                }
            });
            all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            (all, 0)
        }
    };

    if !scan_usage
        && cap.is_some()
        && let Some(cache_mtime) = jsonl_tree_mtime(&root)
    {
        crate::agent_sessions::cache::store_result_with_mtime(
            SessionAgent::Codex,
            cwd,
            cache_mtime,
            &result.0,
            result.1,
        );
    }

    result
}

/// Codex's layout is `YYYY/MM/DD/*.jsonl` - three levels below the root - so
/// a depth bound of 8 leaves generous slack while making a pathologically deep
/// tree (or any symlink cycle that slips past the `file_type` guard) terminate
/// instead of overflowing the stack.
const MAX_WALK_DEPTH: u32 = 8;

/// Walk Codex's `YYYY/MM/DD/*.jsonl` layout depth-first and invoke
/// `visit` on every `.jsonl` leaf.
fn walk_jsonl_files(dir: &Path, visit: &mut impl FnMut(&Path)) {
    walk_jsonl_files_bounded(dir, MAX_WALK_DEPTH, visit);
}

fn walk_jsonl_files_bounded(dir: &Path, depth_left: u32, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // `DirEntry::file_type()` reports the entry's *own* type (from
        // the readdir record, or an lstat) and does NOT follow symlinks -
        // unlike `Path::is_dir()`, which dereferences. A symlinked directory
        // therefore reports as neither dir nor file and is skipped, so a
        // planted cycle (`sessions/loop -> ../../sessions`) can never be
        // descended. Entries whose type can't be read are skipped.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if depth_left > 0 {
                walk_jsonl_files_bounded(&path, depth_left - 1, visit);
            }
        } else if file_type.is_file() && is_jsonl_file(&path) {
            visit(&path);
        }
    }
}

fn jsonl_tree_mtime(root: &Path) -> Option<SystemTime> {
    let mut latest = fs::metadata(root).ok().and_then(|m| m.modified().ok());
    walk_jsonl_files(root, &mut |path| {
        let modified = fs::metadata(path).ok().and_then(|m| m.modified().ok());
        latest = max_mtime(latest, modified);
    });
    latest
}

fn max_mtime(current: Option<SystemTime>, candidate: Option<SystemTime>) -> Option<SystemTime> {
    match (current, candidate) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn is_jsonl_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
}

/// Title-only wrapper, used by the unit tests (production routes through
/// [`read_sessions_for_cwd_inner`] → [`read_session_meta_inner`] directly).
#[cfg(test)]
fn read_session_meta(path: &Path) -> Option<SessionMeta> {
    read_session_meta_inner(path, false, None)
}

/// Read the head of a rollout file: extract the `session_meta` envelope
/// (line 1) and the first user message (typically a few lines later). When
/// `scan_usage` (the attribution path) the tail scan also captures the model
/// (`turn_context`) and the last cumulative `token_count` usage.
///
/// `cwd_filter` short-circuits the scan: Codex's store is one flat date tree
/// for every project, so most files on disk belong to another directory and
/// their body is pure waste to read. Passing the caller's cwd stops those
/// files at line 1, which is what makes [`TITLE_SCAN_BYTES`] affordable.
///
/// Returns `None` for a rollout that is not worth a row: a sub-agent thread
/// (it belongs to its parent) or a session that never ran a turn.
fn read_session_meta_inner(
    path: &Path,
    scan_usage: bool,
    cwd_filter: Option<&str>,
) -> Option<SessionMeta> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();

    // Line 1 must be session_meta or we skip the file.
    buf.clear();
    // Cap line read at
    // SESSION_META_MAX_BYTES. Truncated line fails serde_json parse below
    // and the file is skipped -- same outcome as a malformed line.
    let n = reader
        .by_ref()
        .take(SESSION_META_MAX_BYTES)
        .read_line(&mut buf)
        .ok()?;
    if n == 0 {
        return None;
    }
    if n as u64 == SESSION_META_MAX_BYTES && !buf.ends_with('\n') {
        log::warn!(
            target: "splitlane_app::codex_sessions",
            "session JSONL line truncated at {} bytes for {} -- skipping file",
            SESSION_META_MAX_BYTES,
            path.display(),
        );
        return None;
    }
    let first_value: serde_json::Value = serde_json::from_str(buf.trim_end()).ok()?;
    if first_value.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
        return None;
    }
    let payload = first_value.get("payload")?;
    // A sub-agent gets its own rollout file, tagged `thread_source:"subagent"`
    // with the spawning thread under `payload.source.subagent`. Its
    // `payload.id` is its own, so without this guard every sub-agent turn
    // would list as a session of its own beside the parent that already
    // represents the work.
    if payload.get("thread_source").and_then(|v| v.as_str()) == Some("subagent") {
        return None;
    }
    let session_id = payload.get("id").and_then(|v| v.as_str())?.to_string();
    let cwd = payload.get("cwd").and_then(|v| v.as_str())?.to_string();
    if cwd.is_empty() {
        return None;
    }
    if let Some(want) = cwd_filter
        && !crate::agent_sessions::cwd_matches(&cwd, want)
    {
        return None;
    }
    // session_id lands verbatim in `codex resume <id>`, so hold it to the
    // strict `^[A-Za-z0-9_-]+$` allow-list (Codex ids are UUIDs): rejects a
    // `\r`/`\n` that would submit injected text and a `;`/space that would
    // chain a second shell command. cwd is display-only today but a future
    // `cd <cwd>` prefix would inherit the gap, and a path legitimately carries
    // `/` + spaces, so keep the control-char guard for it. Mirrors (and
    // tightens) the guard in `opencode_sessions::record_to_session`.
    if !crate::agent_sessions::is_valid_session_id(&session_id)
        || cwd.chars().any(|c| c.is_control())
    {
        log::warn!(
            "codex_sessions: dropped {} -- payload carries an invalid id or control chars in cwd",
            path.display(),
        );
        return None;
    }
    // Inner timestamp is the session start; outer envelope timestamp is
    // the moment the file was opened. They're typically within seconds
    // of each other - prefer the inner (session-relative) value.
    let timestamp = payload
        .get("timestamp")
        .and_then(|v| v.as_str())
        .or_else(|| first_value.get("timestamp").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    // Codex records the branch it started on in `session_meta.payload.git`
    // (beside `commit_hash` and `repository_url`). The branch is an
    // attribution ranking tier, never a filter. Control-char guard for the
    // same reason as `cwd` above: it is display-only today.
    let git_branch = payload
        .get("git")
        .and_then(|g| g.get("branch"))
        .and_then(|v| v.as_str())
        .filter(|b| !b.chars().any(char::is_control))
        .unwrap_or("")
        .to_string();

    // Title-only path keeps the scan cheap: bounded by lines and bytes, and it
    // stops at the first labelable prompt. The attribution path runs the
    // deeper tail scan (model + usage).
    let tail = if scan_usage {
        scan_tail_with_usage(&mut reader)
    } else {
        scan_head_for_title(&mut reader)
    };
    let TailScan {
        summary,
        model,
        usage,
        rate_limits,
        saw_activity,
    } = tail;

    // A rollout whose only line is `session_meta` is a thread opened and
    // closed without sending anything. It has no title and nothing to resume,
    // so it takes no row - it would render as a raw id. Keyed on "the scan saw
    // a body record at all" rather than on the summary, so a session whose
    // prompt is unlabelable still gets its row.
    if !saw_activity {
        return None;
    }

    // The limits are read but nothing renders them yet, and saying so out loud
    // is the point of this line rather than an apology for it.
    //
    // Two things are missing before a Codex row can exist, and neither is code
    // in this file. The rail's footer speaks one vendor's shape today; the
    // normalised form that would let it hold two is the limits track's own next
    // step, and deriving it from one source whose fields are all null would be
    // inventing it. And nobody here has seen a populated snapshot: this machine
    // runs Codex on an API key, which has no plan window at all. So this logs
    // what it found, which is the instrument that captures the first real one -
    // `RUST_LOG=splitlane_app::codex_sessions=debug`.
    if let Some(limits) = rate_limits.as_ref() {
        let window = |w: Option<CodexRateLimitWindow>| match w {
            Some(w) => format!(
                "{:.1}% window={:?}m resets_at={:?}",
                w.used_percent, w.window_minutes, w.resets_at
            ),
            None => "absent".to_string(),
        };
        log::debug!(
            target: "splitlane_app::codex_sessions",
            "codex rate limits in {}: id={:?} name={:?} plan={:?} reached={:?} primary=[{}] secondary=[{}]",
            path.display(),
            limits.limit_id,
            limits.limit_name,
            limits.plan_type,
            limits.rate_limit_reached_type,
            window(limits.primary),
            window(limits.secondary),
        );
    }

    Some(SessionMeta {
        agent: SessionAgent::Codex,
        session_id,
        last_activity_secs: crate::agent_sessions::last_activity_secs_for_file(path, &timestamp),
        timestamp,
        cwd,
        git_branch,
        first_prompt: summary.clone(),
        summary,
        model,
        usage,
    })
}

/// What one deep pass over a rollout found.
///
/// A struct rather than the tuple this used to be: it grew a fourth member and
/// four bare `Option`s in a row is a call site nobody can read.
struct TailScan {
    summary: Option<String>,
    model: Option<String>,
    usage: Option<AssistantUsage>,
    /// Read, reported, and **not yet rendered** - see the call site.
    rate_limits: Option<CodexRateLimits>,
    /// Whether any body record was parsed at all - the empty-session signal.
    saw_activity: bool,
}

/// Deeper tail scan for the attribution path. Walks up to
/// [`MODEL_USAGE_SCAN_LIMIT`] lines capturing the first user message (label),
/// the model (`turn_context.payload.model`), and the LAST cumulative
/// `token_count` usage event. Codex reports `token_count` as a running total,
/// so the last one wins (not summed). Usage is normalized to the shared
/// [`AssistantUsage`] tier semantics (input = uncached input, cache_read =
/// cached subset) so the pricing table treats Claude and Codex uniformly.
fn scan_tail_with_usage(reader: &mut BufReader<fs::File>) -> TailScan {
    let mut summary: Option<String> = None;
    let mut model: Option<String> = None;
    let mut usage: Option<AssistantUsage> = None;
    let mut rate_limits: Option<CodexRateLimits> = None;
    let mut saw_activity = false;
    let mut reached_end = false;
    let mut buf = String::new();
    for _ in 0..MODEL_USAGE_SCAN_LIMIT {
        buf.clear();
        let n = match reader.by_ref().take(MAX_LINE_BYTES).read_line(&mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            reached_end = true;
            break;
        }
        let trimmed = buf.trim_end();
        if !trimmed.starts_with('{') {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let record_type = value.get("type").and_then(|v| v.as_str());
        if is_activity_record(record_type) {
            saw_activity = true;
        }
        if summary.is_none() {
            summary = user_text_from_record(&value);
        }
        match record_type {
            Some("turn_context") => {
                if model.is_none()
                    && let Some(m) = value
                        .get("payload")
                        .and_then(|p| p.get("model"))
                        .and_then(|v| v.as_str())
                    && !m.is_empty()
                {
                    model = Some(m.to_string());
                }
            }
            Some("event_msg") => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if payload.get("type").and_then(|v| v.as_str()) == Some("token_count") {
                    // The limits ride beside the usage in the same event,
                    // and like the usage they are a running statement
                    // rather than an increment: the last one that says
                    // anything wins.
                    if let Some(found) = payload.get("rate_limits").and_then(parse_rate_limits) {
                        rate_limits = Some(found);
                    }
                    if let Some(total) =
                        payload.get("info").and_then(|i| i.get("total_token_usage"))
                    {
                        let input_total = total
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let cached = total
                            .get("cached_input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let output = total
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let u = AssistantUsage {
                            input: input_total.saturating_sub(cached),
                            output,
                            cache_read: cached,
                            cache_creation: 0,
                        };
                        // Cumulative - last non-empty wins.
                        if !u.is_empty() {
                            usage = Some(u);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    TailScan {
        summary,
        model,
        usage,
        rate_limits,
        // Only a scan that reached the end of the file can prove it empty; one
        // stopped by its budget has merely not seen a body record yet.
        saw_activity: saw_activity || !reached_end,
    }
}

/// Scan the head of a rollout for the first human-authored message, bounded by
/// [`TITLE_SCAN_LIMIT`] lines AND [`TITLE_SCAN_BYTES`].
///
/// Signature is concrete on `BufReader<File>` rather than the
/// generic `R: BufRead` it used to be: the `by_ref().take()`
/// pattern needed for the per-line byte cap fails to
/// type-check against `&mut R` (the compiler auto-derefs to `R`
/// and the move blocks the borrow). The only call site already
/// passes a `BufReader<File>`, so the generic was vestigial.
fn scan_head_for_title(reader: &mut BufReader<fs::File>) -> TailScan {
    let mut summary = None;
    let mut saw_activity = false;
    let mut reached_end = false;
    let mut buf = String::new();
    let mut budget = TITLE_SCAN_BYTES;
    for _ in 0..TITLE_SCAN_LIMIT {
        if budget == 0 {
            break;
        }
        buf.clear();
        // Cap each line read.
        // Oversize lines fall through to `serde_json::from_str` which
        // errors and the loop `continue`s -- the scan moves on to the
        // next chunk without OOMing.
        let n = match reader
            .by_ref()
            .take(MAX_LINE_BYTES.min(budget))
            .read_line(&mut buf)
        {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            reached_end = true;
            break;
        }
        budget = budget.saturating_sub(n as u64);
        let trimmed = buf.trim_end();
        if !trimmed.starts_with('{') {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if is_activity_record(value.get("type").and_then(|v| v.as_str())) {
            saw_activity = true;
        }
        if let Some(text) = user_text_from_record(&value) {
            summary = Some(text);
            break;
        }
    }
    TailScan {
        summary,
        model: None,
        usage: None,
        rate_limits: None,
        saw_activity: saw_activity || !reached_end,
    }
}

/// Whether this record proves the rollout carries a real turn. Codex writes a
/// `session_meta`-only file for a thread opened and closed without sending
/// anything; any body record at all means the session actually ran.
fn is_activity_record(record_type: Option<&str>) -> bool {
    matches!(record_type, Some("response_item") | Some("event_msg"))
}

/// Extract the first human-authored text carried by one rollout record.
/// Three shapes, all of which appear in the wild:
///
/// 1. `event_msg` / `item_completed` with `item.type == "UserMessage"` - the
///    marker in Codex 0.149.x. It fires once per real user turn and never for
///    an injected envelope.
/// 2. `response_item` / `message` / `role == "user"` with `input_text`
///    content - the same turn, a few lines earlier, and the only shape older
///    rollouts carry. It also carries every injected envelope, hence the
///    [`SYNTHETIC_USER_PREFIXES`] filter in [`clean_user_message`].
/// 3. `event_msg` / `user_message` / `message` - the pre-0.149 shape, and the
///    only one this reader knew. Codex no longer writes it: every rollout on
///    this machine since 0.149.1 (24 August) has none, so each of them had no
///    title. Kept so older sessions keep their label.
fn user_text_from_record(value: &serde_json::Value) -> Option<String> {
    let payload = value.get("payload")?;
    match value.get("type").and_then(|v| v.as_str())? {
        "event_msg" => match payload.get("type").and_then(|v| v.as_str())? {
            "item_completed" => {
                let item = payload.get("item")?;
                if item.get("type").and_then(|v| v.as_str()) != Some("UserMessage") {
                    return None;
                }
                first_labelable_block(item.get("content")?.as_array()?, "text")
            }
            "user_message" => clean_user_message(payload.get("message")?.as_str()?),
            _ => None,
        },
        "response_item" => {
            if payload.get("type").and_then(|v| v.as_str()) != Some("message")
                || payload.get("role").and_then(|v| v.as_str()) != Some("user")
            {
                return None;
            }
            first_labelable_block(payload.get("content")?.as_array()?, "input_text")
        }
        _ => None,
    }
}

/// First content block of type `kind` that survives [`clean_user_message`].
/// Codex packs several blocks into one record and the envelopes come first,
/// so this walks past them rather than judging the record on its first block.
fn first_labelable_block(blocks: &[serde_json::Value], kind: &str) -> Option<String> {
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some(kind))
        .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
        .find_map(clean_user_message)
}

/// Clean one candidate label, rejecting the synthetic envelopes Codex injects
/// as `role:"user"` records around the real prompt.
fn clean_user_message(raw: &str) -> Option<String> {
    let trimmed = raw.trim_start();
    if SYNTHETIC_USER_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return None;
    }
    clean_session_label(raw, LABEL_MAX_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduce the real Codex rollout sequence observed in the wild:
    /// line 1 is `session_meta`, then a few state events, then the first
    /// `event_msg` `user_message`.
    #[test]
    fn read_session_meta_extracts_envelope_and_first_user_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-26T13:11:10.338Z","type":"session_meta","payload":{"id":"019dc9ea-38d7-7372-9cc4-253ce944d41b","timestamp":"2026-04-26T13:11:03.694Z","cwd":"/home/arthur/dev/splitlane","originator":"codex-tui","cli_version":"0.123.0","model_provider":"openai"}}"#,
                "\n",
                r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-26T13:11:10.345Z","type":"event_msg","payload":{"type":"user_message","message":"Explique le projet stp","images":[]}}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        let meta = read_session_meta(&path).expect("envelope extracted");
        assert_eq!(meta.agent, SessionAgent::Codex);
        assert_eq!(meta.session_id, "019dc9ea-38d7-7372-9cc4-253ce944d41b");
        assert_eq!(meta.cwd, "/home/arthur/dev/splitlane");
        assert_eq!(meta.timestamp, "2026-04-26T13:11:03.694Z");
        assert!(meta.git_branch.is_empty());
        assert_eq!(meta.summary.as_deref(), Some("Explique le projet stp"));
    }

    #[test]
    fn usage_scan_captures_model_and_normalizes_token_count() {
        // scan_usage=true captures `turn_context.payload.model`
        // and the LAST cumulative `token_count`, normalized to the shared tier
        // semantics (input = uncached input, cache_read = cached subset). The
        // title-only path leaves model/usage None.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout-usage.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-26T13:11:10.338Z","type":"session_meta","payload":{"id":"019dc9ea-38d7-7372-9cc4-253ce944d41b","timestamp":"2026-04-26T13:11:03.694Z","cwd":"/home/arthur/dev/splitlane"}}"#,
                "\n",
                r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":500,"cached_input_tokens":200,"output_tokens":80}}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900,"cached_input_tokens":300,"output_tokens":150}}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        let title_only = read_session_meta_inner(&path, false, None).expect("meta");
        assert!(title_only.model.is_none());
        assert!(title_only.usage.is_none());

        let with_usage = read_session_meta_inner(&path, true, None).expect("meta");
        assert_eq!(with_usage.model.as_deref(), Some("gpt-5"));
        let usage = with_usage.usage.expect("usage parsed");
        // Last cumulative event wins: 900 total input, 300 cached → 600 uncached.
        assert_eq!(usage.input, 600);
        assert_eq!(usage.cache_read, 300);
        assert_eq!(usage.output, 150);
        assert_eq!(usage.cache_creation, 0);
    }

    /// The account read: the newest rollout that says anything wins, and the
    /// horizon keeps an abandoned one out.
    #[test]
    fn the_account_limits_come_from_the_newest_rollout_that_says_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("2026").join("09").join("09");
        fs::create_dir_all(&root).expect("layout");

        let meta = r#"{"type":"session_meta","payload":{"id":"a","cwd":"/tmp","timestamp":"2026-09-09T10:00:00Z"}}"#;
        let with_windows = r#"{"timestamp":"2026-09-09T10:05:00Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","primary":{"used_percent":63.5,"window_minutes":300,"resets_at":1788000000},"secondary":null,"plan_type":"plus"}}}"#;
        // Written later than the line above, and saying nothing: Codex writes
        // this shape all day, and it must not erase the reading beside it.
        let all_null = r#"{"timestamp":"2026-09-09T10:06:00Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","primary":null,"secondary":null}}}"#;

        let path = root.join("rollout-2026-09-09T10-00-00-a.jsonl");
        fs::write(&path, format!("{meta}\n{with_windows}\n{all_null}\n")).expect("write");

        let found = last_rate_limits_in_tail(&path).expect("a reading");
        let CodexAccountLimits::Windows {
            limits,
            observed_at,
        } = found
        else {
            panic!("the windows win over the empty object written after them");
        };
        assert_eq!(limits.primary.expect("primary").used_percent, 63.5);
        // The moment **Codex** wrote the line, not the moment we read it.
        assert_eq!(
            observed_at,
            chrono::DateTime::parse_from_rfc3339("2026-09-09T10:05:00Z")
                .unwrap()
                .timestamp()
        );
    }

    /// What the account reader actually finds on **this** machine.
    ///
    /// Ignored by default, like the state reader's corpus test beside it: it
    /// reads `~/.codex/sessions`, which is the developer's own and is not in
    /// the repository. Run it with `--ignored --nocapture` when the answer
    /// matters - it is the only thing that can tell a populated plan window
    /// from the all-null object an API key writes, and the difference between
    /// those two is a row on screen.
    #[test]
    #[ignore = "reads the machine's own ~/.codex/sessions"]
    fn the_account_reader_on_the_real_corpus() {
        let found = read_account_rate_limits(Duration::from_secs(365 * 24 * 60 * 60));
        match &found {
            Some(CodexAccountLimits::Windows {
                limits,
                observed_at,
            }) => println!(
                "windows: primary={:?} secondary={:?} plan={:?} observed_at={observed_at}",
                limits.primary, limits.secondary, limits.plan_type
            ),
            Some(CodexAccountLimits::NoPlanWindow { observed_at }) => {
                println!("no plan window, observed_at={observed_at}")
            }
            None => println!("nothing said"),
        }
    }

    /// An account with no plan window says so, and that is not the same answer
    /// as having read nothing: one draws a row, the other draws none.
    #[test]
    fn an_api_key_login_is_a_state_rather_than_an_absence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout-apikey.jsonl");
        // Verbatim from this machine, where Codex runs on an API key.
        fs::write(
            &path,
            "{\"timestamp\":\"2026-09-09T10:06:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":null,\"rate_limits\":{\"limit_id\":\"codex\",\"limit_name\":null,\"primary\":null,\"secondary\":null,\"credits\":null,\"individual_limit\":null,\"spend_control_reached\":null,\"plan_type\":null,\"rate_limit_reached_type\":null}}}\n",
        )
        .expect("write");
        assert!(matches!(
            last_rate_limits_in_tail(&path),
            Some(CodexAccountLimits::NoPlanWindow { .. })
        ));

        // A rollout with no `token_count` at all has said nothing whatsoever,
        // which is the third answer and the one that draws no row.
        let silent = dir.path().join("rollout-silent.jsonl");
        fs::write(
            &silent,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"b\",\"cwd\":\"/tmp\"}}\n",
        )
        .expect("write");
        assert_eq!(last_rate_limits_in_tail(&silent), None);
    }

    #[test]
    fn rate_limits_are_read_in_the_shape_codex_publishes() {
        // Field names are upstream's (`RateLimitSnapshot` / `RateLimitWindow`
        // in codex-rs/protocol/src/protocol.rs), not ours, so this fixture is
        // checkable against the definition rather than against a memory of it.
        let value: serde_json::Value = serde_json::from_str(
            r#"{"limit_id":"codex","limit_name":"Codex","normal_model_slug":"gpt-5",
                "primary":{"used_percent":63.5,"window_minutes":300,"resets_at":1788000000},
                "secondary":{"used_percent":82.0,"window_minutes":10080,"resets_at":1788500000},
                "credits":null,"individual_limit":null,"spend_control_reached":false,
                "plan_type":"plus","rate_limit_reached_type":null}"#,
        )
        .expect("fixture parses");

        let limits = parse_rate_limits(&value).expect("a snapshot with windows is a reading");
        assert_eq!(limits.limit_id.as_deref(), Some("codex"));
        assert_eq!(limits.plan_type.as_deref(), Some("plus"));
        assert_eq!(limits.rate_limit_reached_type, None);

        let primary = limits.primary.expect("primary window");
        assert_eq!(primary.used_percent, 63.5);
        // 300 minutes is what the app hardcodes as "5-hour" on the Claude path.
        assert_eq!(primary.window_minutes, Some(300));
        assert_eq!(primary.resets_at, Some(1_788_000_000));

        let secondary = limits.secondary.expect("secondary window");
        assert_eq!(secondary.used_percent, 82.0);
        assert_eq!(secondary.window_minutes, Some(10_080));
    }

    #[test]
    fn an_all_null_snapshot_is_nothing_said_rather_than_zero_used() {
        // Verbatim from ~/.codex/sessions on the machine this was measured on,
        // where Codex runs on an API key: the object is written on schedule and
        // every member is null, because an API key has no plan window at all.
        // Reading that as a limit would put "0%" on screen about a limit that
        // does not exist.
        let value: serde_json::Value = serde_json::from_str(
            r#"{"limit_id":"codex","limit_name":null,"primary":null,"secondary":null,
                "credits":null,"individual_limit":null,"spend_control_reached":null,
                "plan_type":null,"rate_limit_reached_type":null}"#,
        )
        .expect("fixture parses");
        assert_eq!(parse_rate_limits(&value), None);
    }

    #[test]
    fn a_window_states_its_length_only_when_the_server_did() {
        // `used_percent` is the one member upstream declares non-optional; the
        // other two are `Option`. A window missing the percentage is malformed
        // rather than empty, and reading it as 0% would be the same lie as
        // above.
        let with_only_a_percentage: serde_json::Value =
            serde_json::from_str(r#"{"primary":{"used_percent":12.0}}"#).expect("parses");
        let window = parse_rate_limits(&with_only_a_percentage)
            .expect("a percentage alone is still a window")
            .primary
            .expect("primary");
        assert_eq!(window.used_percent, 12.0);
        assert_eq!(window.window_minutes, None);
        assert_eq!(window.resets_at, None);

        let without_a_percentage: serde_json::Value =
            serde_json::from_str(r#"{"primary":{"window_minutes":300}}"#).expect("parses");
        assert_eq!(parse_rate_limits(&without_a_percentage), None);
    }

    #[test]
    fn the_last_token_count_that_says_anything_wins() {
        // Same rule the usage already follows: the event is a running statement,
        // not an increment. A later event whose snapshot is all null does NOT
        // erase the reading - Codex writes that shape all day.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout-limits.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":10.0,"window_minutes":300}}}}"#,
                "
",
                r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":41.0,"window_minutes":300}}}}"#,
                "
",
                r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":null,"secondary":null}}}"#,
                "
",
            ),
        )
        .expect("write fixture");

        let file = std::fs::File::open(&path).expect("open");
        let mut reader = BufReader::new(file);
        let found = scan_tail_with_usage(&mut reader)
            .rate_limits
            .expect("the populated events are not erased by the empty one");
        assert_eq!(found.primary.expect("primary").used_percent, 41.0);
    }

    #[test]
    fn read_session_meta_returns_none_for_non_session_meta_first_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-codex.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi"}}
"#,
        )
        .expect("write fixture");
        assert!(read_session_meta(&path).is_none());
    }

    #[test]
    fn read_session_meta_returns_none_when_payload_missing_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("no-cwd.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"session_meta","payload":{"id":"x","timestamp":"2026-04-26T13:11:03.694Z"}}
"#,
        )
        .expect("write fixture");
        assert!(read_session_meta(&path).is_none());
    }

    #[test]
    fn user_message_label_is_truncated_with_ellipsis_when_long() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("long-prompt.jsonl");
        let long_prompt = "x".repeat(200);
        let session_meta_line = r#"{"type":"session_meta","payload":{"id":"s","cwd":"/p","timestamp":"2026-04-26T13:00:00Z"}}"#;
        let user_msg_line = format!(
            r#"{{"type":"event_msg","payload":{{"type":"user_message","message":"{long_prompt}"}}}}"#
        );
        std::fs::write(&path, format!("{session_meta_line}\n{user_msg_line}\n"))
            .expect("write fixture");
        let meta = read_session_meta(&path).expect("meta");
        let summary = meta.summary.expect("summary");
        assert_eq!(summary.chars().count(), LABEL_MAX_CHARS + 1);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn user_message_label_collapses_whitespace_and_controls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("messy-prompt.jsonl");
        let session_meta_line = r#"{"type":"session_meta","payload":{"id":"s","cwd":"/p","timestamp":"2026-04-26T13:00:00Z"}}"#;
        let prompt = serde_json::to_string("Explain\n\tthis\u{1b} now").expect("json string");
        let user_msg_line = format!(
            r#"{{"type":"event_msg","payload":{{"type":"user_message","message":{prompt}}}}}"#
        );
        std::fs::write(&path, format!("{session_meta_line}\n{user_msg_line}\n"))
            .expect("write fixture");

        let meta = read_session_meta(&path).expect("meta");
        assert_eq!(meta.summary.as_deref(), Some("Explain this now"));
    }

    #[test]
    fn session_id_control_char_guard() {
        // payload.id carries CR+LF + an injected shell command. Without
        // the guard, the id flows into `codex resume <id>` and submits
        // `rm -rf ~` as a separate PTY command.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("malicious.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"abc\r\nrm -rf ~","cwd":"/tmp/proj","timestamp":"2026-04-26T13:11:03.694Z"}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert!(
            read_session_meta(&path).is_none(),
            "session with control chars in payload.id must be dropped"
        );
    }

    #[test]
    fn session_id_legitimate_uuid_passes_guard() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ok.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"019dc9ea-38d7-7372-9cc4-253ce944d41b","cwd":"/tmp/proj","timestamp":"2026-04-26T13:11:03.694Z"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        let meta = read_session_meta(&path).expect("legitimate UUID must pass the guard");
        assert_eq!(meta.session_id, "019dc9ea-38d7-7372-9cc4-253ce944d41b");
    }

    /// Codex 0.149.1 shape: the real prompt arrives as a `response_item`
    /// `role:"user"` line and again as an `event_msg` `item_completed`
    /// `UserMessage`. Neither was in the matcher, which is why every recent
    /// Codex row had no title. The injected envelopes that precede it must not
    /// win the title race.
    #[test]
    fn read_session_meta_skips_injected_envelopes_and_takes_the_real_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout-0149.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"01a0323c-fb2b-7af3-9386-742cd0cfb4a6","cwd":"/home/user/dev/project","timestamp":"2026-08-24T05:27:32.000Z","thread_source":"user","git":{"branch":"main","commit_hash":"04e0ae0b"}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<app-context>desktop</app-context>"}]}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/home/user/dev/project</cwd>\n</environment_context>"}]}}"#,
                "\n",
                r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>\nnope\n</recommended_plugins>"},{"type":"input_text","text":"# AGENTS.md instructions for /home/user/dev/project"}]}}"##,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix the session titles"}]}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"Fix the session titles"}]}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        let meta = read_session_meta(&path).expect("meta");
        assert_eq!(meta.summary.as_deref(), Some("Fix the session titles"));
        assert_eq!(meta.git_branch, "main");
    }

    /// The `item_completed` marker alone must produce a label: it is the only
    /// user-turn signal a rollout carries once the `response_item` line
    /// outruns the per-line cap.
    #[test]
    fn item_completed_user_message_yields_the_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("item-completed.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"s","cwd":"/p","timestamp":"2026-08-24T05:27:32.000Z"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"ship it"}]}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        let meta = read_session_meta(&path).expect("meta");
        assert_eq!(meta.summary.as_deref(), Some("ship it"));
    }

    /// A sub-agent thread carries its own `payload.id` and would otherwise
    /// list as a session of its own beside its parent.
    #[test]
    fn subagent_rollout_is_not_a_session_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("subagent.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"01a032cf-f359-7571-87e2-fb9c9d351de9","cwd":"/p","timestamp":"2026-08-24T10:08:04.000Z","thread_source":"subagent","source":{"subagent":{"thread_spawn":{"parent_thread_id":"01a032cd-d49f-7732-b6de-ab083bbcca92","depth":1}}}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"review the standards"}]}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert!(read_session_meta(&path).is_none());
    }

    /// A thread opened and closed without a turn is a `session_meta`-only
    /// file. It has no title and nothing to resume.
    #[test]
    fn session_meta_only_rollout_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"01a037fc-4515-7800-9a3c-000000000000","cwd":"/p","timestamp":"2026-08-25T10:14:34.000Z","thread_source":"user"}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert!(read_session_meta(&path).is_none());
    }

    /// A scan stopped by its byte budget has not proven the rollout empty. An
    /// oversized prelude line (a large `# AGENTS.md` envelope) right after
    /// `session_meta` must cost the title, never the row.
    #[test]
    fn a_scan_stopped_by_its_budget_keeps_the_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big-prelude.jsonl");
        let junk = format!("{{{}\n", "a".repeat(TITLE_SCAN_BYTES as usize + 1));
        std::fs::write(
            &path,
            format!(
                "{}\n{junk}",
                r#"{"type":"session_meta","payload":{"id":"s","cwd":"/p","timestamp":"2026-08-24T05:27:32.000Z"}}"#
            ),
        )
        .expect("write fixture");
        let meta = read_session_meta(&path).expect("row kept");
        assert!(meta.summary.is_none());
    }

    /// `cwd_filter` is what keeps the byte budget affordable: a rollout from
    /// another project must be abandoned before its body is read.
    #[test]
    fn cwd_filter_rejects_a_foreign_rollout_at_line_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("other-project.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"s","cwd":"/home/user/dev/other","timestamp":"2026-08-24T05:27:32.000Z"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"hello"}]}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert!(read_session_meta_inner(&path, false, Some("/home/user/dev/project")).is_none());
        assert!(read_session_meta_inner(&path, false, Some("/home/user/dev/other")).is_some());
    }

    #[test]
    fn cwd_control_char_guard() {
        // Same class of injection as session_id, just one field over.
        // cwd is display-only today but a future `cd <cwd>` prefix
        // would inherit the gap without any git-blame signal.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("malicious-cwd.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"019dc9ea-38d7-7372-9cc4-253ce944d41b","cwd":"/tmp/proj\r\nrm -rf ~","timestamp":"2026-04-26T13:11:03.694Z"}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert!(
            read_session_meta(&path).is_none(),
            "session with control chars in cwd must be dropped"
        );
    }

    /// A deep-but-acyclic tree within the depth bound still yields every
    /// real `.jsonl` leaf - the guard must not drop legitimate sessions.
    #[test]
    fn walk_discovers_jsonl_in_deep_acyclic_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Codex's real shape is 3 levels (YYYY/MM/DD); go a little deeper to
        // prove the bound (8) leaves slack.
        let leaf_dir = dir.path().join("2026/06/08/extra");
        std::fs::create_dir_all(&leaf_dir).expect("mkdir -p");
        let jsonl = leaf_dir.join("rollout.jsonl");
        std::fs::write(&jsonl, b"{}\n").expect("write");
        std::fs::write(leaf_dir.join("not-a-session.txt"), b"ignore me").expect("write");

        let mut found = Vec::new();
        walk_jsonl_files(dir.path(), &mut |p| found.push(p.to_path_buf()));
        assert_eq!(found, vec![jsonl], "the one real .jsonl must be discovered");
    }

    /// The depth bound stops recursion past `MAX_WALK_DEPTH`, so an
    /// arbitrarily deep tree terminates rather than overflowing the stack.
    #[test]
    fn walk_stops_past_depth_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Build MAX_WALK_DEPTH + 4 nested dirs, with a .jsonl just past the
        // bound. The walk must terminate and must NOT visit the too-deep file.
        let mut deep = dir.path().to_path_buf();
        for i in 0..(MAX_WALK_DEPTH + 4) {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).expect("mkdir -p");
        std::fs::write(deep.join("too-deep.jsonl"), b"{}\n").expect("write");

        let mut count = 0usize;
        walk_jsonl_files(dir.path(), &mut |_| count += 1);
        assert_eq!(count, 0, "a leaf past the depth bound must not be visited");
    }

    /// A symlink cycle pointing back at an ancestor must not be
    /// descended (it would otherwise recurse forever and stack-overflow).
    /// Unix-only because creating a symlink on Windows needs elevation/dev
    /// mode. The Windows equivalent (NTFS junction / `IO_REPARSE_TAG_*`) is
    /// reported by `DirEntry::file_type()` on the pinned toolchain (Rust 1.95)
    /// with `is_symlink() = true` and `is_dir() = false` for native Win10/11
    /// volumes - so the same `is_dir()` guard skips it. Treated as
    /// inspection-only (no Win symlink CI leg yet); a junction
    /// on a CIFS/remote-mapped volume is the residual gap to revisit if a
    /// Windows integration test lands.
    #[cfg(unix)]
    #[test]
    fn walk_does_not_follow_symlink_cycle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("2026/06/08");
        std::fs::create_dir_all(&real).expect("mkdir -p");
        let jsonl = real.join("rollout.jsonl");
        std::fs::write(&jsonl, b"{}\n").expect("write");
        // sessions/2026/loop -> sessions (points at an ancestor: a cycle).
        std::os::unix::fs::symlink(dir.path(), dir.path().join("2026/loop"))
            .expect("create symlink cycle");

        let mut found = Vec::new();
        walk_jsonl_files(dir.path(), &mut |p| found.push(p.to_path_buf()));
        // Terminates (no stack overflow) and still finds the one real file
        // exactly once - the symlinked directory was never descended.
        assert_eq!(found, vec![jsonl]);
    }
}
