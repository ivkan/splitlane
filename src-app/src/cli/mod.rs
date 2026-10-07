//! `splitlane <verb>` scriptable CLI.
//!
//! Talks to a RUNNING Splitlane instance over the existing IPC JSON-RPC socket
//! (`splitlane-ipc-client`) and exits before any GPUI init. `main.rs` dispatches
//! here only when `argv[1]` names a known verb ([`is_cli_verb`]) - mirroring the
//! `splitlane mcp …` intercept - so every other invocation (no args, unknown
//! args, `--help`/`--version`/`--update-and-exit`) is left untouched and the GUI
//! launch path is preserved. clap therefore never has to own the "no subcommand
//! => launch the GUI" default, and never eats the manually-parsed top-level
//! flags handled above it.

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;
use splitlane_ipc_client::IpcClient;

mod answer_cmd;
mod control_cmds;
mod flow_cmd;
mod flow_spec;
mod read_cmds;
mod selector;
mod send_cmd;
mod session_cmds;
mod up_cmd;
mod wait_cmd;
mod wait_rail;
mod watch_cmd;
mod workspace_spec;

/// Process exit codes. Kept distinct so scripts can branch on the failure
/// kind. clap owns `2` for its own usage/parse errors (and `0` for
/// `--help`/`--version`), so the runtime codes start at `1` and avoid `2`.
pub const EXIT_OK: i32 = 0;
pub const EXIT_RUNTIME: i32 = 1;
pub const EXIT_TARGET: i32 = 3;
/// `wait` reached its deadline without the pattern appearing. Distinct from
/// EXIT_TARGET (no/ambiguous match) and EXIT_RUNTIME (instance down / pane
/// closed) so scripts can tell a timeout apart from a hard failure.
pub const EXIT_TIMEOUT: i32 = 4;
/// `wait --until turn-end`: the agent is waiting for a person. Its own code
/// because the caller must do something different from every other outcome:
/// pass the question on, not retry and not answer it.
pub const EXIT_NEEDS_PERSON: i32 = 5;
/// `wait --until turn-end`: the run failed, or the agent is gone.
pub const EXIT_AGENT_FAILED: i32 = 6;
/// Nothing that reports for the surface can answer the question asked of it:
/// no source that can say a turn ended, or no reader for the agent's
/// conversation. The caller should switch method, not wait longer.
pub const EXIT_NO_TURN_SIGNAL: i32 = 7;
/// A rule refused the request and nothing was done: the target is not a
/// session the caller opened, a limit is reached, a person is being waited
/// on. Retrying the same call gets the same answer.
pub const EXIT_REFUSED: i32 = 8;
/// The target is a session a person opened, and the person is being asked
/// whether the caller may send it messages. Nothing was written. Not a
/// refusal: the answer is still to come, and `wait --until allowed` waits
/// for it.
pub const EXIT_ASKED_PERSON: i32 = 9;

/// The verbs this CLI owns. `main.rs` gates the whole CLI dispatch (and the
/// manual `--help`/`--version` scans) on membership here so the GUI launch
/// path stays byte-for-byte unchanged for any other `argv[1]`.
///
/// The trailing `list_panes`/`read_pane`/`search_pane` are the
/// `splitlane` MCP tool names, accepted as CLI aliases (clap maps each to its
/// canonical subcommand via `#[command(alias = ...)]`) so a lead agent that types
/// the tool name reaches the matching verb instead of tripping the GUI
/// single-instance guard. This list only gates the `main.rs` intercept.
const VERBS: &[&str] = &[
    "ls",
    "read",
    "search",
    "ps",
    "status",
    "new",
    "select",
    "split",
    "add",
    "park",
    "show",
    "close",
    "interrupt",
    "send",
    "up",
    "wait",
    "answer",
    "watch",
    "focus",
    "key",
    "flow",
    "list_panes",
    "read_pane",
    "search_pane",
];

/// True when `argv[1]` names one of our subcommands (including the MCP-tool
/// aliases above).
pub fn is_cli_verb(arg: Option<&str>) -> bool {
    matches!(arg, Some(v) if VERBS.contains(&v))
}

/// True when `argv[1]` is shaped like a subcommand (present, non-empty, and not
/// a `-`/`--` flag) but is NOT one this CLI owns. `main.rs` calls this only
/// AFTER the `mcp`/`hooks`/known-verb intercepts have each had their chance and
/// exited, so a `true` here is an unmistakable typo (`splitlane blah`, a
/// mistyped `splitlane searh`): it prints an actionable "unknown verb" error and
/// exits non-zero instead of falling through to the GUI launch, which would
/// otherwise trip the single-instance guard with no message.
/// A bare `splitlane` (argv[1] is `None`) returns `false` so the GUI still
/// launches, and a leading-`-` token is a flag, not a verb, so it stays on the
/// GUI/global-flag path.
pub fn looks_like_unknown_verb(arg: Option<&str>) -> bool {
    matches!(arg, Some(v) if !v.is_empty() && !v.starts_with('-') && !VERBS.contains(&v))
}

#[derive(Parser, Debug)]
#[command(
    name = "splitlane",
    version,
    about = "Drive a running Splitlane instance from the shell",
    // The GUI launch (no subcommand) is handled in main.rs, never here, so a
    // bare `splitlane` never reaches clap. `Option<Commands>` keeps clap from
    // forcing `subcommand_required` / `arg_required_else_help` regardless.
    subcommand_required = false,
    arg_required_else_help = false
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List terminal surfaces.
    // `list_panes` is the MCP tool name; accept it as a hidden
    // alias so a lead agent can type either.
    #[command(alias = "list_panes")]
    Ls {
        /// Human-readable table instead of the default JSON.
        #[arg(long)]
        human: bool,
    },
    /// Print a pane's scrollback (raw text by default).
    #[command(alias = "read_pane")]
    Read {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Number of trailing lines (server clamps to 1..4000).
        #[arg(long)]
        lines: Option<u64>,
        /// Offset from the end of the buffer.
        #[arg(long)]
        offset: Option<u64>,
        /// Emit the `{text, lines, total_lines, eof}` envelope as JSON.
        #[arg(long)]
        json: bool,
        /// Return raw scrollback, bypassing the anti-injection fence that
        /// otherwise wraps the output as `<untrusted_terminal_output>` (the
        /// fence is on by default; see the ai_injection_fence setting).
        #[arg(long)]
        raw: bool,
    },
    /// Search a pane's scrollback for a substring/pattern.
    #[command(alias = "search_pane")]
    Search {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Pattern to search for.
        pattern: String,
        /// Cap the number of matches (server clamps to 1..1000).
        #[arg(long)]
        max: Option<u64>,
        /// Human-readable lines instead of the default JSON.
        #[arg(long)]
        human: bool,
    },
    /// List running agents across the fleet (pid, tool, state, pane).
    Ps {
        /// Emit the `{agents:[…]}` envelope as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Read one surface's agent state (thinking / waiting / idle / errored / …).
    Status {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Emit the status envelope as JSON instead of a one-line summary.
        #[arg(long)]
        json: bool,
    },
    /// Print the last answer an agent gave, read from the agent's own record
    /// of the conversation (Claude Code and Codex), never from scrollback.
    Answer {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// The `runs_ended` value read before the prompt was sent. The answer
        /// is refused until a run past it has ended, so the answer of the turn
        /// before is not taken for this one's.
        #[arg(long, value_name = "N")]
        after: Option<u64>,
        /// Above this many bytes the answer is written to a file and only its
        /// first lines are printed (default 16384).
        #[arg(long, value_name = "BYTES")]
        max_bytes: Option<usize>,
        /// Write the whole answer to this file as well.
        #[arg(long, value_name = "FILE")]
        out: Option<std::path::PathBuf>,
        /// Print the answer as written, without the
        /// `<untrusted_terminal_output>` fence that otherwise wraps it.
        #[arg(long)]
        raw: bool,
        /// Emit `{outcome, surface_id, agent, runs_ended, text, bytes,
        /// truncated, file}` as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a new workspace.
    New {
        /// Workspace title.
        #[arg(long)]
        name: Option<String>,
        /// Working directory for the first pane (must exist).
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Select a workspace by index.
    Select {
        /// Zero-based workspace index.
        index: u64,
    },
    /// Split the active pane horizontally or vertically.
    Split {
        /// `h`/`horizontal` (panes stacked) or `v`/`vertical` (side by side).
        direction: SplitDir,
        /// Split the pane hosting this target instead of the first leaf.
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        #[arg(long)]
        target: Option<String>,
    },
    /// Open an agent session, the way the launcher does.
    ///
    /// Run from a pane, it needs nothing switched on: the session opens in
    /// that pane's project and belongs to the session that opened it, which
    /// may then write to it. A session opened this way cannot open sessions
    /// itself, and one session may have 8 running at a time. From outside a
    /// pane it requires `SPLITLANE_IPC_ORCHESTRATION=1` on the running
    /// instance (`SPLITLANE_IPC_SCRIPTING=1` with `--submit`).
    Add {
        /// Which agent: `claude_code`, `codex`, ... or the binary name.
        #[arg(long)]
        agent: String,
        /// Name for the session's row and for later targeting.
        #[arg(long)]
        name: Option<String>,
        /// Text to put on the agent's input line once it has started. Not
        /// submitted unless `--submit` is given.
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,
        /// Read the prompt from this file instead.
        #[arg(long, value_name = "PATH")]
        prompt_file: Option<std::path::PathBuf>,
        /// Submit the prompt.
        #[arg(long)]
        submit: bool,
        /// Open the session in the rail without a pane, leaving the layout
        /// as it is.
        #[arg(long, conflicts_with = "pane")]
        parked: bool,
        /// Require a pane, and refuse (exit 8) when none is empty and another
        /// would not fit. Without either flag the session takes a pane when
        /// there is one and the rail when there is not.
        #[arg(long)]
        pane: bool,
        /// Emit `{surface_id, thread_id, agent, tier, session_id, runs_ended,
        /// placement, placement_reason, opened_by}` as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Take a session out of its pane. It keeps running and keeps its row in
    /// the rail, as when its pane is closed by hand.
    ///
    /// For a session the caller opened with `add`; anything else is refused
    /// (exit 8).
    Park {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Emit `{parked, surface_id}` as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Put a session that is in no pane into one.
    ///
    /// For a session the caller opened with `add`. It takes an empty pane, or
    /// a new one where one fits. With neither, it takes the place of another
    /// session the caller opened - the one that has gone longest without
    /// focus - and never the caller's own pane or a pane showing anything
    /// else; if there is none to give up, it is refused (exit 8).
    Show {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Add the pane next to the one showing this target, when a pane can
        /// be added.
        #[arg(long, value_name = "TARGET")]
        beside: Option<String>,
        /// With `--beside`: `h`/`horizontal` (stacked) or `v`/`vertical`
        /// (side by side, the default).
        #[arg(long, requires = "beside")]
        direction: Option<SplitDir>,
        /// Emit `{shown, surface_id, displaced_surface_id}` as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop a session and drop its row - "Delete session", not "close the
    /// pane". The conversation stays in the agent's own history.
    ///
    /// For a session the caller opened with `add`. A session a person opened
    /// is never closed this way, and neither is the caller itself (exit 8).
    Close {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Close it even in the middle of a turn or while it waits for a
        /// person. Without this such a session is refused (exit 8), because
        /// the turn is what would be lost.
        #[arg(long)]
        stop_turn: bool,
        /// Emit `{closed, surface_id, thread_id}` as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop the turn a session is in. The session stays open and can be
    /// sent a new task.
    ///
    /// For a session the caller opened with `add`, on the same terms as
    /// `key`: it writes the agent's own interrupt key, not Ctrl-C. A session
    /// that is waiting for a person is refused (exit 8) - on a permission
    /// question that key is the answer "no". An agent with no known
    /// interrupt key is refused with exit 7.
    ///
    /// Exits 0 when the agent's own record shows the turn was stopped, 1
    /// when no turn was running (nothing is sent), and 4 when the key was
    /// sent and the run did not end as stopped: it finished by itself a
    /// moment earlier, or the time ran out.
    Interrupt {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Seconds to wait for the stop to be confirmed.
        #[arg(long, value_name = "SECONDS", default_value_t = 10)]
        timeout: u64,
        /// Emit `{outcome, surface_id, status, runs_ended, last_outcome}` as
        /// JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inject text into a pane WITHOUT submitting it (human-in-loop).
    ///
    /// A session writes into the sessions it opened with `add` with nothing
    /// switched on, and the text is preceded by a line naming the sender. Any
    /// other target requires `SPLITLANE_IPC_SCRIPTING=1` on the running
    /// instance. A session that is waiting for a person is refused (exit 8).
    /// The text is written with no trailing newline so the user/agent reviews
    /// and presses Enter themselves - unless `--submit` is passed explicitly.
    Send {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Text to inject (no trailing carriage return is added by default).
        text: String,
        /// Send to EVERY pane matching the target (a multi-match selector is
        /// an error without this flag). Prints a `{sent, failed}` report.
        #[arg(long)]
        broadcast: bool,
        /// Submit the text (append a carriage return). Explicit opt-in: this
        /// is the ONLY way the CLI ever submits on the user's behalf, and it
        /// still requires the instance-side scripting gate.
        #[arg(long)]
        submit: bool,
        /// Force bracketed-paste delivery: the text is wrapped in
        /// `ESC[200~`/`ESC[201~` and, with `--submit`, the carriage return is
        /// sent separately after a calibrated delay so a TUI agent does not
        /// swallow it. `--submit` toward an agent pane enables this
        /// automatically; pass `--paste` to force it (e.g. toward a shell) or
        /// `--paste` alone to wrap a non-submitted inject.
        #[arg(long)]
        paste: bool,
        /// Ask the agent to write its complete result to this file and print
        /// `REPORT_DONE <path>` after the file is fully written. The path is
        /// resolved relative to the caller's current directory.
        #[arg(long, value_name = "PATH")]
        report_file: Option<String>,
    },
    /// Give a targeted surface the keyboard focus.
    Focus {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
    },
    /// Send a named keystroke (e.g. `escape`, `ctrl-c`, `tab`) to a pane.
    ///
    /// Allowed and refused on the same terms as `send`. Keystrokes that would
    /// submit a line (`enter`, `ctrl-m`, `ctrl-j`) are refused - submission is
    /// exclusive to `send --submit`.
    Key {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        target: String,
        /// Dash-separated keystroke description ("escape", "ctrl-c", "alt-f").
        keystroke: String,
    },
    /// Run a declarative agent DAG from a `flow.toml` (orchestration engine).
    #[command(subcommand)]
    Flow(FlowCommand),
    /// Spawn a declarative agent workspace from a TOML file ("compose for agents").
    Up {
        /// Path to a `splitlane.workspace.toml` spec.
        file: String,
        /// Validate + print the resolved plan without touching the instance.
        #[arg(long)]
        dry_run: bool,
    },
    /// Block until a pane goes idle, or a regex appears in its output (orchestration).
    Wait {
        /// Target: surface id, name, `cmdline:<substr>`, or `cwd:<path>`.
        /// Note: `cmdline:` matches the full argv on Linux but only the
        /// executable basename on macOS/Windows; prefer `cwd:` or a name for a
        /// portable selector.
        #[arg(long = "match", value_name = "SELECTOR")]
        selector: String,
        /// Regex to wait for in the pane's recent scrollback. Required unless
        /// `--idle` is set. With `--idle` it is an optional sentinel: it is
        /// checked on each new output and EITHER signal (pattern match OR going
        /// idle) returns first.
        #[arg(long, required_unless_present_any = ["idle", "until", "state"])]
        pattern: Option<String>,
        /// Wait until the pane's output goes quiet (no `output_generation`
        /// change for `--for` ms) by subscribing to the push stream - zero
        /// client-side polling. Single-target.
        #[arg(long)]
        idle: bool,
        /// With `--idle`: the quiescence window in milliseconds (default 1000).
        /// The pane must produce no new output for this long to count as idle.
        #[arg(long = "for", value_name = "MS")]
        for_ms: Option<u64>,
        /// Max seconds to wait before giving up (default 300).
        #[arg(long)]
        timeout: Option<u64>,
        /// Succeed as soon as ANY matching pane matches (selector may hit
        /// several). `--pattern` mode only; ignored with `--idle`.
        #[arg(long, conflicts_with = "all")]
        any: bool,
        /// Require ALL matching panes to match the pattern. `--pattern` mode only.
        #[arg(long)]
        all: bool,
        /// Wait for what the rail says rather than for output: `turn-end`
        /// returns when the agent's turn is over (exit 0), when it is waiting
        /// for a person (5), when the run failed (6), or when nothing can
        /// report a turn ending for that surface (7). Works with `--any` and
        /// `--all`.
        #[arg(long, value_enum, conflicts_with_all = ["idle", "pattern", "for_ms", "state"])]
        until: Option<WaitUntil>,
        /// Wait until the rail's status word is one of these, comma separated:
        /// starting, running, waiting, idle, failed.
        #[arg(long, value_name = "WORDS", conflicts_with_all = ["idle", "pattern", "for_ms"])]
        state: Option<String>,
        /// With `--until turn-end`: the `runs_ended` value read before the
        /// prompt was sent (`send --submit` prints it). The wait returns once
        /// a run past it has ended.
        #[arg(long, value_name = "N", requires = "until")]
        after: Option<u64>,
        /// With `--until turn-end`: keep waiting while the agent waits for a
        /// person, instead of returning with exit 5.
        #[arg(long, requires = "until")]
        through_waiting: bool,
        /// With `--until turn-end`: seconds a surface is given to start a turn
        /// or to gain a source that can report one ending (default 10).
        #[arg(long, value_name = "SECS", requires = "until")]
        start_grace: Option<u64>,
    },
    /// Stream lifecycle events from the running instance as JSONL.
    Watch {
        /// Only stream events for this pane (selector). Omit for all panes.
        #[arg(long)]
        surface: Option<String>,
        /// Only stream these event types (repeatable). Omit for all types.
        #[arg(long = "type", value_name = "TYPE")]
        types: Vec<String>,
        /// Hide subscription protocol frames and print user events only.
        #[arg(long)]
        events_only: bool,
    },
}

#[derive(Subcommand, Debug)]
enum FlowCommand {
    /// Execute (or validate with --dry-run) a flow file against the running
    /// instance. Spawns panes, waits on `ready` barriers, feeds steps -
    /// submission only with explicit `submit = true` + the scripting gate.
    Run {
        /// Path to a `flow.toml`.
        file: String,
        /// Validate + print the resolved plan without touching the instance.
        #[arg(long)]
        dry_run: bool,
        /// Final machine-readable report on stdout (live transitions move to
        /// stderr).
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum SplitDir {
    #[value(name = "horizontal", alias = "h")]
    Horizontal,
    #[value(name = "vertical", alias = "v")]
    Vertical,
}

impl SplitDir {
    /// The `direction` string the `surface.split` IPC method expects.
    fn as_ipc(self) -> &'static str {
        match self {
            SplitDir::Horizontal => "horizontal",
            SplitDir::Vertical => "vertical",
        }
    }
}

/// What `wait --until` waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum WaitUntil {
    /// The agent's turn is over.
    TurnEnd,
    /// A person has answered whether the caller may send messages to the
    /// session: 0 when they allowed it, 8 when they did not, 4 when the time
    /// ran out with the question still standing.
    Allowed,
}

/// A CLI failure carrying the process exit code to surface for it.
#[derive(Debug)]
pub struct CliError {
    pub code: i32,
    pub message: String,
}

impl CliError {
    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_RUNTIME,
            message: message.into(),
        }
    }

    pub fn target(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_TARGET,
            message: message.into(),
        }
    }
}

/// Entry point invoked by `main.rs` when `argv[1]` is a known verb. Parses the
/// args with clap, opens a client to the running instance, dispatches, and
/// returns the process exit code.
pub fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // clap prints `--help`/`--version` (exit 0) and usage errors (exit 2)
        // itself; we just relay its code rather than letting `parse()` abort
        // the process from inside a GUI binary.
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };

    // `command` is always `Some` here: main.rs only calls `run` when argv[1]
    // is a known verb. The `None` arm is unreachable in practice.
    let Some(command) = cli.command else {
        return EXIT_OK;
    };

    let client = match connect() {
        Ok(client) => client,
        Err(message) => {
            eprintln!("{message}");
            return EXIT_RUNTIME;
        }
    };

    match dispatch(command, &client) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("splitlane: {}", e.message);
            e.code
        }
    }
}

/// Resolve the socket path and build a client. The path is resolved eagerly
/// (honoring `SPLITLANE_SOCKET_PATH`), but a missing instance only surfaces as
/// an "unreachable … is Splitlane running?" error on the first `call`, so a
/// resolvable-but-dead socket is not a `connect` failure.
fn connect() -> Result<IpcClient, String> {
    let socket = splitlane_ipc_client::resolve_socket_path().ok_or_else(|| {
        "splitlane: cannot locate the IPC socket; is Splitlane running? \
         (set SPLITLANE_SOCKET_PATH if you launched the CLI outside a Splitlane pane)"
            .to_string()
    })?;
    Ok(IpcClient::new(socket))
}

/// Route a parsed subcommand to its handler: `read`/`search` + the target
/// selector, `new`/`select`/`split`, `send`, and the rest of the verbs.
fn dispatch(command: Commands, client: &IpcClient) -> Result<i32, CliError> {
    match command {
        Commands::Ls { human } => read_cmds::ls(client, human),
        Commands::Read {
            target,
            lines,
            offset,
            json,
            raw,
        } => read_cmds::read(client, &target, lines, offset, json, raw),
        Commands::Search {
            target,
            pattern,
            max,
            human,
        } => read_cmds::search(client, &target, &pattern, max, human),
        Commands::Ps { json } => read_cmds::ps(client, json),
        Commands::Status { target, json } => read_cmds::status(client, &target, json),
        Commands::Answer {
            target,
            after,
            max_bytes,
            out,
            raw,
            json,
        } => answer_cmd::answer(
            client,
            &target,
            answer_cmd::AnswerOptions {
                after,
                max_bytes: max_bytes.unwrap_or(answer_cmd::DEFAULT_MAX_BYTES),
                out,
                raw,
                json,
            },
        ),
        Commands::New { name, cwd } => {
            control_cmds::new_workspace(client, name.as_deref(), cwd.as_deref())
        }
        Commands::Select { index } => control_cmds::select(client, index),
        Commands::Split { direction, target } => {
            control_cmds::split(client, direction.as_ipc(), target.as_deref())
        }
        Commands::Add {
            agent,
            name,
            prompt,
            prompt_file,
            submit,
            parked,
            pane,
            json,
        } => session_cmds::add(
            client,
            session_cmds::AddOptions {
                agent,
                name,
                prompt,
                prompt_file,
                submit,
                parked,
                pane,
                json,
            },
        ),
        Commands::Park { target, json } => session_cmds::park(client, &target, json),
        Commands::Close {
            target,
            stop_turn,
            json,
        } => session_cmds::close(client, &target, stop_turn, json),
        Commands::Interrupt {
            target,
            timeout,
            json,
        } => session_cmds::interrupt(
            client,
            &target,
            std::time::Duration::from_secs(timeout),
            json,
        ),
        Commands::Show {
            target,
            beside,
            direction,
            json,
        } => session_cmds::show(
            client,
            &target,
            beside.as_deref(),
            direction.map(SplitDir::as_ipc),
            json,
        ),
        Commands::Send {
            target,
            text,
            broadcast,
            submit,
            paste,
            report_file,
        } => send_cmd::send(
            client,
            &target,
            &text,
            broadcast,
            submit,
            paste,
            report_file.as_deref(),
        ),
        Commands::Focus { target } => control_cmds::focus(client, &target),
        Commands::Key { target, keystroke } => send_cmd::key(client, &target, &keystroke),
        Commands::Flow(FlowCommand::Run {
            file,
            dry_run,
            json,
        }) => flow_cmd::run(client, &file, dry_run, json),
        Commands::Up { file, dry_run } => up_cmd::up(client, &file, dry_run),
        Commands::Wait {
            selector,
            pattern,
            idle,
            for_ms,
            timeout,
            any,
            all,
            until,
            state,
            after,
            through_waiting,
            start_grace,
        } => {
            let mode = if all {
                wait_cmd::MatchMode::All
            } else if any {
                wait_cmd::MatchMode::Any
            } else {
                wait_cmd::MatchMode::Single
            };
            if until == Some(WaitUntil::Allowed) {
                if after.is_some() || through_waiting || start_grace.is_some() || any || all {
                    return Err(CliError::runtime(
                        "--until allowed waits on one session and takes only --timeout",
                    ));
                }
                let (timeout, _) = wait_rail::durations(timeout, None);
                return session_cmds::wait_allowed(client, &selector, timeout);
            }
            let rail_wait = match (until, state.as_deref()) {
                (Some(WaitUntil::Allowed), _) => None,
                (Some(WaitUntil::TurnEnd), _) => Some(wait_rail::RailWait::TurnEnd),
                (None, Some(words)) => Some(wait_rail::RailWait::State(
                    wait_rail::parse_state_words(words)?,
                )),
                (None, None) => None,
            };
            // clap drops `requires = "until"` when `--state` is present,
            // because the two conflict; so the pairing is checked here.
            if until != Some(WaitUntil::TurnEnd)
                && (after.is_some() || through_waiting || start_grace.is_some())
            {
                return Err(CliError::runtime(
                    "--after, --through-waiting and --start-grace apply to --until turn-end only",
                ));
            }
            if let Some(wait) = rail_wait {
                let (timeout, start_grace) = wait_rail::durations(timeout, start_grace);
                wait_rail::wait_rail(
                    client,
                    &selector,
                    wait_rail::RailWaitOptions {
                        wait,
                        after,
                        through_waiting,
                        start_grace,
                        timeout,
                        mode,
                    },
                )
            } else if idle {
                // Push-based quiescence; optional sentinel.
                wait_cmd::wait_idle(client, &selector, for_ms, timeout, pattern.as_deref())
            } else {
                // Pattern-only poll path (unchanged). clap's
                // `required_unless_present="idle"` guarantees `Some` here; guard
                // defensively so a future flag change can't smuggle an empty
                // (match-everything) regex through.
                let Some(pattern) = pattern else {
                    return Err(CliError::runtime(
                        "wait requires --pattern <regex> unless --idle is set",
                    ));
                };
                wait_cmd::wait(client, &selector, &pattern, timeout, mode)
            }
        }
        Commands::Watch {
            surface,
            types,
            events_only,
        } => watch_cmd::watch(client, surface.as_deref(), &types, events_only),
    }
}

/// Render a JSON-RPC `result` value as pretty JSON to stdout. Shared by the
/// read and control command modules so every machine-readable output uses one
/// renderer.
pub(super) fn print_json(value: &Value) -> Result<(), CliError> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|e| CliError::runtime(format!("failed to render JSON: {e}")))?;
    println!("{rendered}");
    Ok(())
}

/// Reject a server reply that carries a *legacy* application error.
///
/// A handful of server handlers signal cap/validation failures (split at
/// `MAX_PANES`, `select` out-of-range, `send_text` over the 64 KiB limit) with
/// an ad-hoc `{"error": "<message>"}` payload that does NOT use the
/// `_jsonrpc_error` sentinel. The dispatcher therefore promotes them under
/// `result`, so the transport's `parse_response` returns `Ok` and the command
/// would otherwise print the error and exit 0 - breaking the scriptability
/// contract, which promises a non-zero exit code on failure. Calling this on every
/// `result` before printing maps that legacy shape to a non-zero `CliError`.
///
/// No success envelope on these verbs carries a top-level `error` string
/// (`{index,…}`, `{selected}`, `{split,…}`, `{sent,…}`, `{surfaces,…}`,
/// `{text,…}`, `{matches,…}`), so the check can't false-positive on real data.
pub(super) fn reject_legacy_error(result: Value) -> Result<Value, CliError> {
    if let Some(message) = result.get("error").and_then(Value::as_str) {
        return Err(CliError::runtime(message.to_string()));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The skill shipped with the app teaches an agent this CLI, and an
    /// agent does what it is taught: a verb or a flag that is not there is
    /// a command it will run and fail on, every time. Each `splitlane` line
    /// in the skill's shell blocks is checked against the real definitions.
    #[test]
    fn every_command_the_skill_teaches_exists() {
        use clap::CommandFactory;
        let skill = include_str!("../../../skills/splitlane-fleet/SKILL.md");
        let cli = Cli::command();
        let mut in_shell_block = false;
        let mut checked = 0;
        let mut missing = Vec::new();
        for line in skill.lines() {
            let line = line.trim();
            if line.starts_with("```") {
                in_shell_block = line == "```bash";
                continue;
            }
            let Some(rest) = line.strip_prefix("splitlane ").filter(|_| in_shell_block) else {
                continue;
            };
            let words: Vec<&str> = rest
                .split_whitespace()
                .take_while(|word| !matches!(*word, "|" | "#" | ">" | "&&"))
                .collect();
            let Some(mut command) = words.first().and_then(|verb| cli.find_subcommand(verb)) else {
                missing.push(format!("no such verb: {line}"));
                continue;
            };
            if let Some(nested) = words.get(1).and_then(|word| command.find_subcommand(word)) {
                command = nested;
            }
            for flag in words.iter().filter_map(|word| word.strip_prefix("--")) {
                if !command
                    .get_arguments()
                    .any(|arg| arg.get_long() == Some(flag))
                {
                    missing.push(format!("no flag --{flag}: {line}"));
                }
            }
            checked += 1;
        }
        assert!(missing.is_empty(), "{missing:#?}");
        assert!(checked >= 30, "only {checked} commands were found to check");
    }

    #[test]
    fn is_cli_verb_matches_known_verbs() {
        assert!(is_cli_verb(Some("ls")));
        assert!(is_cli_verb(Some("send")));
        assert!(is_cli_verb(Some("focus")));
        assert!(is_cli_verb(Some("key")));
        assert!(!is_cli_verb(Some("mcp")));
        assert!(!is_cli_verb(Some("--version")));
        assert!(!is_cli_verb(None));
    }

    #[test]
    fn mcp_tool_names_alias_to_their_verbs() {
        // The MCP tool names gate the CLI dispatch in main.rs...
        assert!(is_cli_verb(Some("search_pane")));
        assert!(is_cli_verb(Some("read_pane")));
        assert!(is_cli_verb(Some("list_panes")));
        // ...and clap routes each alias to its canonical subcommand, so a
        // lead agent that types the MCP name never lands on the GUI launch path.
        let cli = Cli::try_parse_from(["splitlane", "search_pane", "backend", "needle"])
            .expect("parse search_pane");
        assert!(matches!(cli.command, Some(Commands::Search { .. })));
        let cli =
            Cli::try_parse_from(["splitlane", "read_pane", "backend"]).expect("parse read_pane");
        assert!(matches!(cli.command, Some(Commands::Read { .. })));
        let cli = Cli::try_parse_from(["splitlane", "list_panes"]).expect("parse list_panes");
        assert!(matches!(cli.command, Some(Commands::Ls { .. })));
    }

    #[test]
    fn unknown_verb_detected_but_bare_and_flags_are_not() {
        // A verb-shaped typo is flagged so main.rs errors
        // actionably instead of launching the GUI / tripping the singleton.
        assert!(looks_like_unknown_verb(Some("blah")));
        assert!(looks_like_unknown_verb(Some("searh")));
        // Known verbs and MCP aliases are NOT unknown.
        assert!(!looks_like_unknown_verb(Some("search")));
        assert!(!looks_like_unknown_verb(Some("search_pane")));
        assert!(!looks_like_unknown_verb(Some("ls")));
        // A bare `splitlane` (None) and an empty token still launch the GUI.
        assert!(!looks_like_unknown_verb(None));
        assert!(!looks_like_unknown_verb(Some("")));
        // Flags stay on the global-flag / GUI path, never the unknown-verb error.
        assert!(!looks_like_unknown_verb(Some("--help")));
        assert!(!looks_like_unknown_verb(Some("-v")));
        assert!(!looks_like_unknown_verb(Some("--update-and-exit")));
    }

    #[test]
    fn ps_parses_with_optional_json_flag() {
        let cli = Cli::try_parse_from(["splitlane", "ps", "--json"]).expect("parse");
        assert!(matches!(cli.command, Some(Commands::Ps { json: true })));
        // Default is the human table (like Unix `ps`), JSON is opt-in.
        let cli = Cli::try_parse_from(["splitlane", "ps"]).expect("parse");
        assert!(matches!(cli.command, Some(Commands::Ps { json: false })));
    }

    #[test]
    fn status_requires_a_target() {
        let err = Cli::try_parse_from(["splitlane", "status"]).expect_err("usage");
        assert_eq!(err.exit_code(), 2);
        let cli = Cli::try_parse_from(["splitlane", "status", "backend"]).expect("parse");
        assert!(matches!(cli.command, Some(Commands::Status { .. })));
    }

    #[test]
    fn send_flags_default_off() {
        // The human-in-loop default: no broadcast, no submit, no paste unless
        // explicit (absent `--paste`, the server auto-decides).
        let cli = Cli::try_parse_from(["splitlane", "send", "backend", "hi"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Send {
                broadcast: false,
                submit: false,
                paste: false,
                ..
            })
        ));
        let cli = Cli::try_parse_from(["splitlane", "send", "--broadcast", "--submit", "sh", "go"])
            .expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Send {
                broadcast: true,
                submit: true,
                paste: false,
                ..
            })
        ));
        // `--paste` is an explicit, parseable override.
        let cli =
            Cli::try_parse_from(["splitlane", "send", "--paste", "agent", "hi"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Send { paste: true, .. })
        ));
        let cli = Cli::try_parse_from([
            "splitlane",
            "send",
            "--report-file",
            "reports/out.md",
            "agent",
            "hi",
        ])
        .expect("parse");
        assert!(
            matches!(cli.command, Some(Commands::Send { report_file: Some(p), .. }) if p == "reports/out.md")
        );
    }

    #[test]
    fn split_target_is_optional() {
        let cli = Cli::try_parse_from(["splitlane", "split", "v"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Split { target: None, .. })
        ));
        let cli =
            Cli::try_parse_from(["splitlane", "split", "v", "--target", "backend"]).expect("parse");
        assert!(
            matches!(cli.command, Some(Commands::Split { target: Some(t), .. }) if t == "backend")
        );
    }

    #[test]
    fn key_requires_target_and_keystroke() {
        let err = Cli::try_parse_from(["splitlane", "key", "backend"]).expect_err("usage");
        assert_eq!(err.exit_code(), 2);
        let cli = Cli::try_parse_from(["splitlane", "key", "backend", "escape"]).expect("parse");
        assert!(matches!(cli.command, Some(Commands::Key { .. })));
    }

    #[test]
    fn answer_parsing() {
        let cli = Cli::try_parse_from([
            "splitlane",
            "answer",
            "reviewer",
            "--after",
            "7",
            "--max-bytes",
            "4096",
            "--raw",
        ])
        .expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Answer {
                after: Some(7),
                max_bytes: Some(4096),
                raw: true,
                json: false,
                ..
            })
        ));
        let err = Cli::try_parse_from(["splitlane", "answer"]).expect_err("usage");
        assert_eq!(err.exit_code(), 2);
    }

    /// The rail modes parse without `--pattern`, carry their own flags, and
    /// refuse to be mixed with the output-based modes.
    #[test]
    fn wait_rail_modes_parsing() {
        let cli = Cli::try_parse_from([
            "splitlane",
            "wait",
            "--match",
            "reviewer",
            "--until",
            "turn-end",
            "--after",
            "7",
            "--through-waiting",
            "--all",
        ])
        .expect("--until turn-end parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Wait {
                until: Some(WaitUntil::TurnEnd),
                after: Some(7),
                through_waiting: true,
                all: true,
                pattern: None,
                ..
            })
        ));
        let cli = Cli::try_parse_from([
            "splitlane",
            "wait",
            "--match",
            "a",
            "--state",
            "idle,waiting",
        ])
        .expect("--state parses");
        assert!(
            matches!(cli.command, Some(Commands::Wait { state: Some(s), until: None, .. }) if s == "idle,waiting")
        );
        for bad in [
            vec!["--until", "turn-end", "--idle"],
            vec!["--until", "turn-end", "--pattern", "DONE"],
            vec!["--until", "turn-end", "--state", "idle"],
            vec!["--until", "quiet"],
        ] {
            let mut argv = vec!["splitlane", "wait", "--match", "a"];
            argv.extend(bad.iter().copied());
            assert!(Cli::try_parse_from(argv).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn wait_idle_and_pattern_parsing() {
        // `--idle` parses WITHOUT `--pattern` (the sentinel is
        // optional in idle mode); `--for` carries the quiescence window.
        let cli = Cli::try_parse_from([
            "splitlane",
            "wait",
            "--match",
            "agent",
            "--idle",
            "--for",
            "500",
        ])
        .expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Wait {
                idle: true,
                pattern: None,
                for_ms: Some(500),
                ..
            })
        ));
        // `--idle` + `--pattern` coexist (OR semantics, first to fire).
        let cli = Cli::try_parse_from([
            "splitlane",
            "wait",
            "--match",
            "a",
            "--idle",
            "--pattern",
            "DONE",
        ])
        .expect("parse");
        assert!(
            matches!(cli.command, Some(Commands::Wait { idle: true, pattern: Some(p), .. }) if p == "DONE")
        );
        // `--pattern` alone (no `--idle`) still parses (the existing poll path).
        let cli = Cli::try_parse_from(["splitlane", "wait", "--match", "a", "--pattern", "DONE"])
            .expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Wait {
                idle: false,
                pattern: Some(_),
                ..
            })
        ));
        // Neither `--idle` nor `--pattern` -> clap usage error (exit 2), never a
        // silent empty-regex that matches everything.
        let err = Cli::try_parse_from(["splitlane", "wait", "--match", "a"]).expect_err("usage");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn watch_parses_optional_surface_and_repeatable_types() {
        let cli = Cli::try_parse_from(["splitlane", "watch"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Watch { surface: None, .. })
        ));
        let cli = Cli::try_parse_from([
            "splitlane",
            "watch",
            "--surface",
            "backend",
            "--type",
            "ai.stop",
            "--type",
            "ai.notification",
        ])
        .expect("parse");
        match cli.command {
            Some(Commands::Watch {
                surface,
                types,
                events_only,
            }) => {
                assert_eq!(surface.as_deref(), Some("backend"));
                assert_eq!(types, vec!["ai.stop", "ai.notification"]);
                assert!(!events_only);
            }
            other => panic!("expected Watch, got {other:?}"),
        }
    }

    #[test]
    fn watch_parses_events_only() {
        let cli = Cli::try_parse_from(["splitlane", "watch", "--events-only"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Watch {
                events_only: true,
                ..
            })
        ));
    }

    #[test]
    fn cli_parses_a_verb_with_flags() {
        let cli = Cli::try_parse_from(["splitlane", "ls", "--human"]).expect("parse");
        assert!(matches!(cli.command, Some(Commands::Ls { human: true })));
    }

    #[test]
    fn split_accepts_short_aliases() {
        let cli = Cli::try_parse_from(["splitlane", "split", "h"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Split {
                direction: SplitDir::Horizontal,
                ..
            })
        ));
    }

    #[test]
    fn read_requires_a_target() {
        // Missing the required positional `target` is a clap usage error (2).
        let err = Cli::try_parse_from(["splitlane", "read"]).expect_err("usage");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn no_subcommand_parses_to_none() {
        // Defensive: a bare invocation never reaches `run` (main.rs gates on a
        // known verb), but clap must not force-error on it.
        let cli = Cli::try_parse_from(["splitlane"]).expect("parse");
        assert!(cli.command.is_none());
    }
}
