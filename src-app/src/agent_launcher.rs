//! Terminal-agent launcher: the CLI coding agents Splitlane starts in a
//! terminal pane (Claude Code, Codex, OpenCode, Pi, Hermes, plus the
//! cmux-derived set: Grok, Amp, Cursor, Gemini, Kiro, Antigravity,
//! Copilot, CodeBuddy, Factory, Qoder, plus Openclaw). Both the tab-bar
//! launcher buttons
//! (`pane.rs`) and the Agents-view "New thread" picker iterate this single
//! source of truth so the per-agent visibility gate and the "respect
//! bypass" contract can never drift between them.
//!
//! Each variant maps to a display name, an icon, an accent tint, a
//! Settings → AI Agent visibility flag (`*_button_visible`), a stable
//! persistence tag, and a launch command. The launch command honors
//! `claude_code_bypass_permissions` exactly as the tab bar does.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use splitlane_config::schema::SplitlaneConfig;

/// How a thread's forced agent session id reaches the CLI.
///
/// Claude Code separates *minting* a session from *resuming* one, and the two
/// are not interchangeable (verified against Claude Code 2.1.232):
///
/// - `--session-id <uuid>` - "Use a specific session ID for the conversation".
///   Names a **new** conversation. Once that id has a session file on disk the
///   CLI refuses the launch with
///   `Error: Session ID <uuid> is already in use.`
/// - `-r, --resume <uuid>` - "Resume a conversation by session ID". The only
///   way back into an existing session.
///
/// A thread therefore mints on the launch that creates its session and
/// resumes on every launch after that. Passing `--session-id` unconditionally
/// works on first mount and breaks on every reopen, which is what
/// [`SessionBinding::resolve`] exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBinding<'a> {
    /// No forced id: bare shell, non-Claude agent, or a thread that never
    /// minted one.
    Unbound,
    /// The id has no session file yet - mint it (`--session-id <uuid>`).
    Mint(&'a str),
    /// The id already has a session file - reattach (`--resume <uuid>`).
    Resume(&'a str),
}

impl<'a> SessionBinding<'a> {
    /// Resolve the binding for `session_id` in `cwd` by asking the on-disk
    /// session store whether the CLI has seen this id before.
    ///
    /// Disk is the authority rather than any in-memory "have we launched yet"
    /// flag: it stays correct when the user deletes a session behind
    /// Splitlane's back, when `session.json` is restored onto a different
    /// machine, and when the CLI never got far enough to write the file.
    pub fn resolve(session_id: Option<&'a str>, cwd: &str) -> Self {
        match session_id {
            Some(id) if crate::claude_sessions::session_file_exists(cwd, id) => Self::Resume(id),
            Some(id) => Self::Mint(id),
            None => Self::Unbound,
        }
    }
}

/// One of the CLI coding agents Splitlane can launch in a terminal.
///
/// Distinct from [`splitlane_acp::AgentKind`] (Claude/Codex only, the ACP
/// wire agents): this is the broader set surfaced as terminal launchers
/// and bound to Agents-view Terminal Threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalAgent {
    ClaudeCode,
    Codex,
    OpenCode,
    Pi,
    Hermes,
    Grok,
    Amp,
    Cursor,
    Gemini,
    Kiro,
    Antigravity,
    Copilot,
    CodeBuddy,
    Factory,
    Qoder,
    Openclaw,
}

/// The one word beside an agent's name wherever an agent is chosen.
///
/// Four words for sixteen agents, and every agent carries one: an empty chip
/// reads as "unknown", and a rule saying "no chip means launch only" is a rule
/// the user has to remember. See [`TerminalAgent::capability_tier`] for why the
/// assignment is derived from this codebase rather than copied from the
/// design's list.
///
/// There were five. `CanIntervene` - "can intervene" - named the rung an agent
/// reached by having its permission asks held open and answered inside
/// Splitlane, and that is gone: an agent asks in its own terminal. The one agent
/// that sat on it (Codex) reads one rung down, on `ResumeByName`, which is what
/// the code still earns for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityTier {
    FullControl,
    ResumeByName,
    HistoryOnly,
    LaunchOnly,
}

impl CapabilityTier {
    pub fn word(self) -> &'static str {
        match self {
            CapabilityTier::FullControl => "full control",
            CapabilityTier::ResumeByName => "resume by name",
            CapabilityTier::HistoryOnly => "history only",
            CapabilityTier::LaunchOnly => "launch only",
        }
    }

    /// Whether the word is worth advertising. The top rung is what the design
    /// puts in the accent; the rest are stated, not sold.
    ///
    /// It used to be the top **two**, and the second of them was "can
    /// intervene". With that rung gone the accent is on the one claim this
    /// build makes about following an agent completely.
    pub fn is_advertised(self) -> bool {
        matches!(self, CapabilityTier::FullControl)
    }
}

impl TerminalAgent {
    /// Every variant, in display order (matches the tab-bar button row).
    /// The original five lead; the cmux-derived launchers follow so the
    /// button order is stable for users who upgraded from a 5-agent build.
    pub const ALL: [TerminalAgent; 16] = [
        TerminalAgent::ClaudeCode,
        TerminalAgent::Codex,
        TerminalAgent::OpenCode,
        TerminalAgent::Pi,
        TerminalAgent::Hermes,
        TerminalAgent::Grok,
        TerminalAgent::Amp,
        TerminalAgent::Cursor,
        TerminalAgent::Gemini,
        TerminalAgent::Kiro,
        TerminalAgent::Antigravity,
        TerminalAgent::Copilot,
        TerminalAgent::CodeBuddy,
        TerminalAgent::Factory,
        TerminalAgent::Qoder,
        TerminalAgent::Openclaw,
    ];

    /// Stable display rank - index in [`Self::ALL`]. Used by the sidebar to
    /// order multi-tool status rows deterministically instead of letting
    /// `HashMap` iteration order leak into the UI.
    pub fn display_rank(self) -> usize {
        Self::ALL
            .iter()
            .position(|a| *a == self)
            .unwrap_or(usize::MAX)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            TerminalAgent::ClaudeCode => "Claude Code",
            TerminalAgent::Codex => "Codex",
            TerminalAgent::OpenCode => "OpenCode",
            TerminalAgent::Pi => "Pi",
            TerminalAgent::Hermes => "Hermes Agent",
            TerminalAgent::Grok => "Grok",
            TerminalAgent::Amp => "Amp",
            TerminalAgent::Cursor => "Cursor",
            TerminalAgent::Gemini => "Gemini",
            TerminalAgent::Kiro => "Kiro",
            TerminalAgent::Antigravity => "Antigravity",
            TerminalAgent::Copilot => "Copilot",
            TerminalAgent::CodeBuddy => "CodeBuddy",
            TerminalAgent::Factory => "Factory",
            TerminalAgent::Qoder => "Qoder",
            TerminalAgent::Openclaw => "Openclaw",
        }
    }

    pub fn icon_path(self) -> &'static str {
        match self {
            TerminalAgent::ClaudeCode => "icons/claude-color.svg",
            TerminalAgent::Codex => "icons/codex-color.svg",
            TerminalAgent::OpenCode => "icons/opencode-color.svg",
            TerminalAgent::Pi => "icons/pi-coding-agent.svg",
            TerminalAgent::Hermes => "icons/hermesagent.svg",
            TerminalAgent::Grok => "agents/grok.svg",
            TerminalAgent::Amp => "agents/amp-color.svg",
            TerminalAgent::Cursor => "agents/cursor.svg",
            TerminalAgent::Gemini => "agents/gemini-color.svg",
            TerminalAgent::Kiro => "agents/kiro-color.svg",
            TerminalAgent::Antigravity => "agents/antigravity-color.svg",
            TerminalAgent::Copilot => "agents/githubcopilot.svg",
            TerminalAgent::CodeBuddy => "agents/codebuddy-color.svg",
            TerminalAgent::Factory => "agents/factory.svg",
            TerminalAgent::Qoder => "agents/qoder-color.svg",
            TerminalAgent::Openclaw => "agents/openclaw-color.svg",
        }
    }

    /// Brand accent for the icon tint, as a packed `0xRRGGBB`. `None`
    /// means "use the theme's primary text color" -- the OpenCode / Pi /
    /// Hermes logos are monochrome `currentColor` SVGs.
    pub fn accent(self) -> Option<u32> {
        match self {
            TerminalAgent::ClaudeCode => Some(0xd97757),
            TerminalAgent::Codex => Some(0x7a9dff),
            // Single-color brand logos: `svg()` renders a monochrome alpha
            // mask, so the silhouette is painted in this brand color.
            TerminalAgent::Amp => Some(0xF34E3F),
            TerminalAgent::Qoder => Some(0x2ADB5C),
            // The rest are either monochrome `currentColor` logos (tinted
            // with the theme's primary text color so they stay readable on
            // every theme) or multi-color logos rendered in their native
            // palette via `img()` (see `icon_multicolor`), where `accent`
            // is unused.
            TerminalAgent::OpenCode
            | TerminalAgent::Pi
            | TerminalAgent::Hermes
            | TerminalAgent::Grok
            | TerminalAgent::Cursor
            | TerminalAgent::Gemini
            | TerminalAgent::Kiro
            | TerminalAgent::Antigravity
            | TerminalAgent::Copilot
            | TerminalAgent::CodeBuddy
            | TerminalAgent::Factory
            | TerminalAgent::Openclaw => None,
        }
    }

    /// Whether the icon must be rendered in its native colors via `img()`
    /// (multi-color logos: gradients or several distinct fills) instead of
    /// a `text_color`-tinted monochrome `svg()` mask. GPUI's `svg()`
    /// flattens every path to one tint, which would destroy these palettes;
    /// `img()` rasterizes the SVG (resvg) and preserves every fill. A
    /// single-color brand logo stays monochrome and uses `accent()`.
    pub fn icon_multicolor(self) -> bool {
        matches!(
            self,
            TerminalAgent::Antigravity
                | TerminalAgent::CodeBuddy
                | TerminalAgent::Gemini
                | TerminalAgent::Kiro
                | TerminalAgent::Openclaw
        )
    }

    /// Stable persistence tag for the session.json `terminal_agent`
    /// field. Kept distinct from the binary name so a future rename of
    /// the CLI does not invalidate persisted threads.
    pub fn tag(self) -> &'static str {
        match self {
            TerminalAgent::ClaudeCode => "claude_code",
            TerminalAgent::Codex => "codex",
            TerminalAgent::OpenCode => "opencode",
            TerminalAgent::Pi => "pi",
            TerminalAgent::Hermes => "hermes",
            TerminalAgent::Grok => "grok",
            TerminalAgent::Amp => "amp",
            TerminalAgent::Cursor => "cursor",
            TerminalAgent::Gemini => "gemini",
            TerminalAgent::Kiro => "kiro",
            TerminalAgent::Antigravity => "antigravity",
            TerminalAgent::Copilot => "copilot",
            TerminalAgent::CodeBuddy => "codebuddy",
            TerminalAgent::Factory => "factory",
            TerminalAgent::Qoder => "qoder",
            TerminalAgent::Openclaw => "openclaw",
        }
    }

    /// Map an ACP [`splitlane_acp::AgentKind`] (Claude/Codex only) to its
    /// terminal launcher. Used to relaunch legacy chat threads (which
    /// stored an `AgentKind`) as terminal sessions of the same agent.
    pub fn from_agent_kind(kind: splitlane_acp::AgentKind) -> TerminalAgent {
        match kind {
            splitlane_acp::AgentKind::ClaudeCode => TerminalAgent::ClaudeCode,
            splitlane_acp::AgentKind::Codex => TerminalAgent::Codex,
        }
    }

    /// Map a detected process basename back to its agent
    /// (reverse of [`Self::binary`]). Exact match only - the per-pane scan
    /// matches `/proc/<pid>/comm` verbatim, so a wrapper script or a
    /// suffixed binary never produces a pill.
    pub fn from_binary(name: &str) -> Option<TerminalAgent> {
        TerminalAgent::ALL
            .iter()
            .copied()
            .find(|a| a.binary() == name)
    }

    pub fn from_tag(tag: &str) -> Option<TerminalAgent> {
        match tag {
            "claude_code" => Some(TerminalAgent::ClaudeCode),
            "codex" => Some(TerminalAgent::Codex),
            "opencode" => Some(TerminalAgent::OpenCode),
            "pi" => Some(TerminalAgent::Pi),
            "hermes" => Some(TerminalAgent::Hermes),
            "grok" => Some(TerminalAgent::Grok),
            "amp" => Some(TerminalAgent::Amp),
            "cursor" => Some(TerminalAgent::Cursor),
            "gemini" => Some(TerminalAgent::Gemini),
            "kiro" => Some(TerminalAgent::Kiro),
            "antigravity" => Some(TerminalAgent::Antigravity),
            "copilot" => Some(TerminalAgent::Copilot),
            "codebuddy" => Some(TerminalAgent::CodeBuddy),
            "factory" => Some(TerminalAgent::Factory),
            "qoder" => Some(TerminalAgent::Qoder),
            "openclaw" => Some(TerminalAgent::Openclaw),
            _ => None,
        }
    }

    /// Whether this launcher is offered where a new agent is started - the
    /// empty pane's launcher, and the workspace-template pane editor.
    ///
    /// Historical note, because the name still says otherwise: the key is
    /// `*_button_visible` because these used to be buttons in every pane's tab
    /// bar. That cluster was removed with the rest of the old cockpit; the
    /// gate outlived it.
    ///
    /// Tri-state on the `*_button_visible` config key:
    /// - `Some(true)`  - user explicitly enabled it: always shown.
    /// - `Some(false)` - user explicitly disabled it: always hidden.
    /// - `None` (key absent, the default) - shown only if the agent's CLI
    ///   binary is installed ([`Self::is_installed`]), so a fresh config
    ///   surfaces exactly the agents present on the machine. The user can
    ///   still force-show an uninstalled agent by toggling it on.
    pub fn is_visible(self, config: &SplitlaneConfig) -> bool {
        let explicit: Option<bool> = match self {
            TerminalAgent::ClaudeCode => config.claude_code_button_visible,
            TerminalAgent::Codex => config.codex_button_visible,
            TerminalAgent::OpenCode => config.opencode_button_visible,
            TerminalAgent::Pi => config.pi_button_visible,
            TerminalAgent::Hermes => config.hermes_agent_button_visible,
            TerminalAgent::Grok => config.grok_button_visible,
            TerminalAgent::Amp => config.amp_button_visible,
            TerminalAgent::Cursor => config.cursor_button_visible,
            TerminalAgent::Gemini => config.gemini_button_visible,
            TerminalAgent::Kiro => config.kiro_button_visible,
            TerminalAgent::Antigravity => config.antigravity_button_visible,
            TerminalAgent::Copilot => config.copilot_button_visible,
            TerminalAgent::CodeBuddy => config.codebuddy_button_visible,
            TerminalAgent::Factory => config.factory_button_visible,
            TerminalAgent::Qoder => config.qoder_button_visible,
            TerminalAgent::Openclaw => config.openclaw_button_visible,
        };
        explicit.unwrap_or_else(|| self.is_installed())
    }

    /// The top-level config key [`Self::is_visible`] reads. Settings writes
    /// through it, so the two cannot drift into naming different keys for the
    /// same agent.
    pub fn visibility_key(self) -> &'static str {
        match self {
            TerminalAgent::ClaudeCode => "claude_code_button_visible",
            TerminalAgent::Codex => "codex_button_visible",
            TerminalAgent::OpenCode => "opencode_button_visible",
            TerminalAgent::Pi => "pi_button_visible",
            TerminalAgent::Hermes => "hermes_agent_button_visible",
            TerminalAgent::Grok => "grok_button_visible",
            TerminalAgent::Amp => "amp_button_visible",
            TerminalAgent::Cursor => "cursor_button_visible",
            TerminalAgent::Gemini => "gemini_button_visible",
            TerminalAgent::Kiro => "kiro_button_visible",
            TerminalAgent::Antigravity => "antigravity_button_visible",
            TerminalAgent::Copilot => "copilot_button_visible",
            TerminalAgent::CodeBuddy => "codebuddy_button_visible",
            TerminalAgent::Factory => "factory_button_visible",
            TerminalAgent::Qoder => "qoder_button_visible",
            TerminalAgent::Openclaw => "openclaw_button_visible",
        }
    }

    /// The CLI executable looked up on `PATH` to decide default visibility;
    /// also the leading token of [`Self::launch_command`]. Cross-platform:
    /// `which` resolves Windows `.exe`/`PATHEXT` extensions.
    pub fn binary(self) -> &'static str {
        match self {
            TerminalAgent::ClaudeCode => "claude",
            TerminalAgent::Codex => "codex",
            TerminalAgent::OpenCode => "opencode",
            TerminalAgent::Pi => "pi",
            TerminalAgent::Hermes => "hermes",
            TerminalAgent::Grok => "grok",
            TerminalAgent::Amp => "amp",
            TerminalAgent::Cursor => "cursor-agent",
            TerminalAgent::Gemini => "gemini",
            TerminalAgent::Kiro => "kiro-cli",
            TerminalAgent::Antigravity => "agy",
            TerminalAgent::Copilot => "copilot",
            TerminalAgent::CodeBuddy => "codebuddy",
            TerminalAgent::Factory => "droid",
            TerminalAgent::Qoder => "qodercli",
            TerminalAgent::Openclaw => "openclaw",
        }
    }

    /// Whether this agent's CLI binary is found on `PATH`. Drives the
    /// default visibility in [`Self::is_visible`].
    ///
    /// Probed through a short-lived process cache: `which` walks `PATH` for
    /// every agent, too costly to repeat on the render thread each frame, but
    /// a process-lifetime cache would hide agents installed after startup.
    pub fn is_installed(self) -> bool {
        installed_binaries_contains(self.binary())
    }

    /// Static arguments appended after [`Self::binary`] for interactive agents
    /// whose CLI entry point is a subcommand rather than the bare executable.
    fn command_args(self) -> &'static [&'static str] {
        match self {
            TerminalAgent::Kiro => &["chat"],
            TerminalAgent::Openclaw => &["tui"],
            _ => &[],
        }
    }

    fn launch_spec(self, config: &SplitlaneConfig) -> AgentCommandSpec {
        let mut spec = AgentCommandSpec::new(self.binary());
        spec.extend_args(self.command_args().iter().copied());
        if self == TerminalAgent::ClaudeCode
            && config.claude_code_bypass_permissions.unwrap_or(false)
        {
            spec.push_arg("--permission-mode");
            spec.push_arg("bypassPermissions");
        }
        spec
    }

    /// Bare command that starts the agent. Honors
    /// `claude_code_bypass_permissions` for Claude Code.
    pub(crate) fn command(self, config: &SplitlaneConfig) -> String {
        self.launch_spec(config).render_shell_command()
    }

    /// Whether the CLI accepts a caller-forced session UUID via
    /// `--session-id <uuid>`. Only Claude Code does, and only for a *fresh*
    /// id - an id that already has a session file must be reattached with
    /// `--resume` instead. See [`SessionBinding`] for the full contract.
    /// Other agents learn their id the other way round: the CLI picks it and
    /// reports it back in its `SessionStart` hook, which `ai.session_start`
    /// binds to the surface. Forcing an id and being told one arrive at the
    /// same place - a surface that knows which session file is its own.
    ///
    /// Historical note, because this comment said otherwise and the claim
    /// outlived the code: the fallback used to be described as "the
    /// newest-session heuristic in the title backfill". That heuristic is
    /// gone - `title_summary_for_bound_session` requires an exact id match,
    /// and a test says so by name - and the sentence survived to mislead a
    /// design discussion months later.
    pub fn supports_forced_session_id(self) -> bool {
        matches!(self, TerminalAgent::ClaudeCode)
    }

    /// or waiting for a person - from the file the agent writes for itself.
    ///
    /// This is the predicate the product now rests on, in place of intercepting
    /// permission asks: a transcript the CLI writes anyway plus the liveness of
    /// its process, neither of which a vendor has to keep giving us
    /// (`src-app/src/agent_state.rs`, which also records how it was measured).
    ///
    /// One home, not two: [`crate::agent_sessions::state_file_for`] asks this
    /// rather than naming an agent of its own, so the word in Settings and the
    /// pass that fills the rail change together or not at all.
    ///
    /// **This is not "this build can parse that agent's conversation".** The two
    /// were the same predicate while there was one reader, and they are not the
    /// same fact: the model badge and "copy the last answer" want a *Claude-
    /// shaped* transcript, and handing them a Codex rollout would have them
    /// report "no answer yet" about a session that has plenty - a claim about
    /// that session rather than about the limits of this build. That question
    /// has its own predicate now, [`Self::conversation_is_readable`].
    ///
    /// # Adding an agent here needs the process chain measured first
    ///
    /// The rule's other input is the worker, and until 26 August this doc
    /// carried a warning against Codex on exactly that ground: Codex installed
    /// through npm runs as `shell → shim → node → codex`, so a resolver that
    /// stepped over the shim to name the agent landed on the wrapper and
    /// reported a worker for ever.
    ///
    /// **That is closed, not waived.** The worker is no longer "a child of the
    /// agent" but "a descendant of the PTY child that was not there when the
    /// turn ended" (`65eb4fe`): [`crate::process_tree::worker_against`] and
    /// `WorkerBaseline::take` are both rooted at the PTY child, so the npm
    /// wrapper sits *inside* the baseline exactly like our own shim does - it
    /// is furniture, and furniture is not a worker. Naming the agent is not
    /// part of the answer any more.
    pub fn reports_state(self) -> bool {
        matches!(self, TerminalAgent::ClaudeCode | TerminalAgent::Codex)
    }

    /// Whether this build can parse this agent's **conversation** - the answers
    /// it wrote, and the model it wrote them with.
    ///
    /// Claude Code alone, and deliberately narrower than [`Self::reports_state`].
    /// Reading a session's state is a question about a handful of records near
    /// the end of a file; reading its conversation is a question about the whole
    /// shape of that file, and the two agents' files agree about neither. The
    /// model badge (`app::pane_header`) and "Copy the last answer"
    /// (`app::event_handlers`) ask this one.
    pub fn conversation_is_readable(self) -> bool {
        matches!(self, TerminalAgent::ClaudeCode)
    }

    /// Whether Splitlane can reopen one of this agent's past sessions by its id.
    ///
    /// Derived rather than listed: `sessions_sidebar::resume_command_spec` is
    /// total over [`crate::agent_sessions::SessionAgent`], so an agent whose
    /// sessions Splitlane can read is an agent Splitlane can resume. A test asserts
    /// the two stay in step, because the day someone adds a reader without a
    /// resume arm is the day this quietly starts lying.
    pub fn can_resume_by_name(self) -> bool {
        self.session_agent().is_some()
    }

    /// The one word this agent's chip carries wherever an agent is chosen.
    ///
    /// # Why it is derived and not transcribed
    ///
    /// The design assigns five words by name: full control
    /// (Claude Code) · can intervene (Codex) · resume by name (opencode) ·
    /// history only (Gemini, Pi, Hermes, Grok, Cursor, Kiro) · launch only (the
    /// remaining seven). This build says four of them: "can intervene" named
    /// answering an agent's permission ask in Splitlane, which it no longer
    /// does. Six of the assignments are wrong **about this build** too: every
    /// agent whose sessions Splitlane lists, it can also reopen by id - all nine
    /// of them, `resume_command_renders_expected_agent_commands` names each
    /// one. Transcribing the list would put "history only" on a row the app
    /// can resume, which is the precise failure `CLAUDE.md`'s "one word, one
    /// meaning" section is about: a name that is right in the design and wrong
    /// in the app.
    ///
    /// So the words are the designer's and the assignment is the code's.
    /// `HistoryOnly` has no members today and stays a variant all the same: it
    /// is a real rung, and an agent with a reader and no resume lands on it the
    /// moment one exists.
    ///
    /// # Why the top rung takes two predicates
    ///
    /// "Full control" is a claim about knowing where an agent is at any moment,
    /// and it takes both halves: a session Splitlane can pin, so it knows **which**
    /// file is this surface's, and a reader for that file, so it knows what the
    /// agent is doing in it. Either alone leaves a gap the word does not admit
    /// to, so an agent with only one of them lands on `resume by name`, which
    /// is exactly what it does. A word the code has not earned is
    /// simply not said.
    ///
    /// # Why the top rung stopped being about intervening
    ///
    /// It used to read `supports_forced_session_id() && intervention() ==
    /// Wired`, which was true of the same one agent and meant something else:
    /// that Splitlane holds this agent's permission asks. It does not - answering
    /// happens in the agent's own terminal, and the contour that held an ask
    /// open is gone. The same agent earns the rung for a better reason: Splitlane
    /// follows it completely, and can say so without a hook staying wired.
    pub fn capability_tier(self) -> CapabilityTier {
        if self.supports_forced_session_id() && self.reports_state() {
            CapabilityTier::FullControl
        } else if self.can_resume_by_name() {
            CapabilityTier::ResumeByName
        } else if self.session_agent().is_some() {
            CapabilityTier::HistoryOnly
        } else {
            CapabilityTier::LaunchOnly
        }
    }

    /// Map this launcher to the session reader Splitlane can safely use.
    /// `None` means the CLI does not expose a documented local list+resume
    /// contract suitable for the sidebar yet.
    pub fn session_agent(self) -> Option<crate::agent_sessions::SessionAgent> {
        use crate::agent_sessions::SessionAgent;
        match self {
            TerminalAgent::ClaudeCode => Some(SessionAgent::Claude),
            TerminalAgent::Codex => Some(SessionAgent::Codex),
            TerminalAgent::OpenCode => Some(SessionAgent::OpenCode),
            TerminalAgent::Pi => Some(SessionAgent::Pi),
            TerminalAgent::Hermes => Some(SessionAgent::Hermes),
            TerminalAgent::Grok => Some(SessionAgent::Grok),
            TerminalAgent::Cursor => Some(SessionAgent::Cursor),
            TerminalAgent::Gemini => Some(SessionAgent::Gemini),
            TerminalAgent::Kiro => Some(SessionAgent::Kiro),
            _ => None,
        }
    }

    /// Like [`Self::command`] but injects the session flag selected by
    /// `binding` for Claude when its id passes the PTY allow-list:
    /// `--session-id <uuid>` to mint, `--resume <uuid>` to reattach. The flag
    /// lands right after the binary so it composes with the optional
    /// `--permission-mode bypassPermissions` already baked into the base
    /// command. Any other agent (or [`SessionBinding::Unbound`]) yields the
    /// plain base command.
    fn command_with_session(self, config: &SplitlaneConfig, binding: SessionBinding<'_>) -> String {
        if self != TerminalAgent::ClaudeCode {
            return self.command(config);
        }
        let (flag, id) = match binding {
            SessionBinding::Unbound => return self.command(config),
            SessionBinding::Mint(id) => ("--session-id", id),
            SessionBinding::Resume(id) => ("--resume", id),
        };
        if !crate::agent_sessions::is_valid_session_id(id) {
            // Every path that can set a thread's id already re-gates on this
            // allow-list (the transcript scan, the session.json restore, the
            // sidebar's resume builder), so reaching here means one of them
            // regressed. The fallback itself is right - launch a working agent
            // rather than refuse to open - but it costs the user the binding:
            // the thread starts a fresh conversation and can never resume the
            // one it names. Say so, or the failure is invisible.
            log::warn!(
                "agent launch: dropping session binding, id failed the allow-list \
                 (thread starts an unbound session)"
            );
            return self.command(config);
        }
        let mut spec = self.launch_spec(config);
        spec.insert_arg(0, flag);
        spec.insert_arg(1, id);
        spec.render_shell_command()
    }

    /// Shell-aware launch command. The clear prefix is selected for the
    /// configured shell (`clear`, `cls`, or `Clear-Host`) so the agent TUI owns
    /// the viewport from the first frame on every platform.
    pub fn launch_command(self, config: &SplitlaneConfig) -> String {
        self.launch_command_with_session(config, SessionBinding::Unbound)
    }

    /// [`Self::launch_command`] with a bound agent session (Claude only - see
    /// [`Self::command_with_session`]). The Agents-view PTY mount resolves
    /// the thread's `session_id` through [`SessionBinding::resolve`] and
    /// passes the result here, so the live thread maps 1:1 to its on-disk
    /// session file on the first launch and reattaches to it on every reopen.
    pub fn launch_command_with_session(
        self,
        config: &SplitlaneConfig,
        binding: SessionBinding<'_>,
    ) -> String {
        // Trim + drop-empty exactly like the PTY session does when it
        // resolves the shell (`pty_session.rs:442`). A config such as
        // `"default_shell": "  pwsh  "` otherwise reaches `clear_then`
        // untrimmed, fails the `which::which` probe, falls back to `cmd.exe`,
        // and emits the wrong clear arm (`cls && claude` for a POSIX command).
        let shell = config
            .default_shell
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        crate::terminal::shell::clear_then(&self.command_with_session(config, binding), shell)
    }

    /// Visible variants for the given config, in display order. Drives
    /// both the Agents-view picker and (via the same gates) the tab bar.
    pub fn visible(config: &SplitlaneConfig) -> Vec<TerminalAgent> {
        TerminalAgent::ALL
            .into_iter()
            .filter(|a| a.is_visible(config))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentCommandSpec {
    program: &'static str,
    args: Vec<String>,
}

impl AgentCommandSpec {
    pub(crate) fn new(program: &'static str) -> Self {
        Self {
            program,
            args: Vec::new(),
        }
    }

    pub(crate) fn push_arg(&mut self, arg: impl Into<String>) {
        self.args.push(arg.into());
    }

    fn insert_arg(&mut self, index: usize, arg: impl Into<String>) {
        self.args.insert(index, arg.into());
    }

    fn extend_args(&mut self, args: impl IntoIterator<Item = &'static str>) {
        self.args.extend(args.into_iter().map(str::to_string));
    }

    pub(crate) fn render_shell_command(&self) -> String {
        debug_assert!(is_plain_shell_token(self.program));
        let mut command = self.program.to_string();
        for arg in &self.args {
            debug_assert!(is_plain_shell_token(arg));
            command.push(' ');
            command.push_str(arg);
        }
        command
    }
}

pub(crate) fn is_plain_shell_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'='))
}

struct InstalledBinaryCache {
    checked_at: Option<Instant>,
    found: HashSet<&'static str>,
}

impl InstalledBinaryCache {
    fn refresh(&mut self) {
        self.found = TerminalAgent::ALL
            .into_iter()
            .map(TerminalAgent::binary)
            .filter(|bin| which::which(bin).is_ok())
            .collect();
        self.checked_at = Some(Instant::now());
    }

    fn is_stale(&self) -> bool {
        self.checked_at
            .is_none_or(|checked_at| checked_at.elapsed() >= INSTALLED_BINARIES_TTL)
    }
}

const INSTALLED_BINARIES_TTL: Duration = Duration::from_secs(2);

fn installed_binary_cache() -> &'static Mutex<InstalledBinaryCache> {
    static CACHE: OnceLock<Mutex<InstalledBinaryCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(InstalledBinaryCache {
            checked_at: None,
            found: HashSet::new(),
        })
    })
}

/// Agent binaries found on `PATH`. The cache is short-lived rather than
/// process-lifetime so agents installed while Splitlane is open can appear
/// without a restart, while render paths avoid re-walking `PATH` every frame.
fn installed_binaries_contains(binary: &'static str) -> bool {
    let mut cache = match installed_binary_cache().lock() {
        Ok(cache) => cache,
        Err(poisoned) => {
            tracing::warn!(
                target: "splitlane_app::agent_launcher",
                "installed binary cache mutex poisoned; refreshing recovered state"
            );
            poisoned.into_inner()
        }
    };
    if cache.is_stale() {
        cache.refresh();
    }
    cache.found.contains(binary)
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    /// The tier is derived, and this is the derivation's one load-bearing
    /// assumption: an agent whose sessions Splitlane reads is an agent Splitlane can
    /// reopen. `sessions_sidebar::resume_command_spec` is total over
    /// `SessionAgent`, so the two move together - and the day someone adds a
    /// reader without a resume arm, `can_resume_by_name` starts lying and this
    /// is what says so.
    #[test]
    fn every_agent_with_a_session_reader_can_be_reopened_by_name() {
        let cfg = splitlane_config::schema::SplitlaneConfig::default();
        for agent in TerminalAgent::ALL {
            let Some(session_agent) = agent.session_agent() else {
                assert!(
                    !agent.can_resume_by_name(),
                    "{} claims resume with no reader behind it",
                    agent.display_name()
                );
                continue;
            };
            assert!(
                crate::app::sessions_sidebar::resume_command(
                    session_agent,
                    "019dc9ea-38d7-7372-9cc4-253ce944d41b",
                    &cfg,
                )
                .is_some(),
                "{} has a reader and no way to resume - its tier is now wrong",
                agent.display_name()
            );
        }
    }

    /// Every agent carries a word, and only the top two are advertised. An
    /// empty chip reads as "unknown", which is the state the fifth word exists
    /// to remove.
    ///
    /// One is advertised, and it is the one rung this build can earn: a session
    /// Splitlane can pin plus a file Splitlane can read. There were two, and the
    /// second was "can intervene" - the rung an agent reached by having its
    /// permission asks answered here.
    #[test]
    fn every_agent_carries_exactly_one_word() {
        let advertised: Vec<&str> = TerminalAgent::ALL
            .iter()
            .filter(|a| a.capability_tier().is_advertised())
            .map(|a| a.display_name())
            .collect();
        assert_eq!(advertised, vec!["Claude Code"]);
        for agent in TerminalAgent::ALL {
            assert!(!agent.capability_tier().word().is_empty());
        }
    }

    /// The word is never one the code has not earned. The top rung takes both
    /// halves - a session Splitlane can pin **and** a file Splitlane can read - and an
    /// agent with one of them reads one rung down.
    #[test]
    fn the_top_rung_needs_both_halves() {
        assert!(TerminalAgent::ClaudeCode.supports_forced_session_id());
        assert_eq!(
            TerminalAgent::ClaudeCode.capability_tier(),
            CapabilityTier::FullControl,
            "both halves are in place - a pinned session and a readable file"
        );
        assert_eq!(
            TerminalAgent::Codex.capability_tier(),
            CapabilityTier::ResumeByName,
            "Codex has a reader but no session Splitlane can pin, and the top rung \
             takes both - so it lands one below, with a readable history"
        );
        assert_eq!(
            TerminalAgent::Gemini.capability_tier(),
            CapabilityTier::ResumeByName,
            "the rung an agent with neither half but a readable history lands on"
        );
    }

    /// The top rung is about following an agent, not about holding its asks:
    /// it takes a session Splitlane can pin **and** a file Splitlane can read.
    #[test]
    fn the_top_rung_is_earned_by_observation() {
        assert!(TerminalAgent::ClaudeCode.reports_state());
        assert_eq!(
            TerminalAgent::ClaudeCode.capability_tier(),
            CapabilityTier::FullControl
        );
        // Codex is the case the two-predicate rung was written for, and it took
        // a second reader to produce one. Splitlane **can** read what Codex is
        // doing (27 August, `codex_state`), and still does not reach the top:
        // Codex names its own session, so Splitlane cannot pin which file is this
        // surface's before the agent says. One half is not the word.
        assert!(TerminalAgent::Codex.reports_state());
        assert!(!TerminalAgent::Codex.supports_forced_session_id());
        assert_eq!(
            TerminalAgent::Codex.capability_tier(),
            CapabilityTier::ResumeByName
        );
        // The agents whose state files this build reads. Not a count for its own
        // sake: `agent_sessions::state_file_for` has an arm per reader, and
        // `every_agent_that_reports_state_has_a_reader_and_the_reverse` fails
        // the build if the two ever disagree.
        assert_eq!(
            TerminalAgent::ALL
                .iter()
                .filter(|a| a.reports_state())
                .count(),
            2
        );
        // And the narrower question stays narrow: reading a conversation is not
        // reading a state, and only one file shape is understood that deeply.
        assert_eq!(
            TerminalAgent::ALL
                .iter()
                .filter(|a| a.conversation_is_readable())
                .count(),
            1
        );
    }
}

/// What the new-agent chord starts in one project - the value the design
/// calls `defaultAgent`.
///
/// **Three states, and the third is the one an `Option<TerminalAgent>` could
/// not hold.** `None` is "this project has never been asked", which inherits
/// the app-level answer; [`PreferredAgent::Ask`] is a person's deliberate
/// choice to be asked every time. Collapsing the two is what made the app-level
/// value unreachable - a project could say "ask" or name an agent, and never
/// "whatever the app says".
///
/// **Nothing writes it but a choice whose whole purpose is to write it.**
/// Launching an agent - from the rail's `+ agent`, from a pane's
/// launcher, from the palette - never does. The rule earns its own note
/// because the opposite is so easy to write: the `+ agent` menu used to set the
/// project's default as a side effect of every pick, so one cross-vendor run
/// through Codex silently and permanently re-pointed the chord, which punishes
/// exactly the person a manager of several agents is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredAgent {
    /// Ask which agent, every time.
    Ask,
    /// Start this one.
    Agent(TerminalAgent),
}

impl PreferredAgent {
    /// The persisted form: the agent's own tag, or `ask`.
    pub fn tag(self) -> &'static str {
        match self {
            PreferredAgent::Ask => "ask",
            PreferredAgent::Agent(agent) => agent.tag(),
        }
    }

    /// Read it back. An unknown tag - an agent this build dropped, a
    /// hand-edited file - is `None`, which is "never asked" and therefore falls
    /// back to the app-level answer rather than to a guess.
    pub fn from_tag(tag: &str) -> Option<Self> {
        if tag == "ask" {
            return Some(PreferredAgent::Ask);
        }
        TerminalAgent::from_tag(tag).map(PreferredAgent::Agent)
    }

    /// The agent this starts, or `None` for "ask".
    pub fn agent(self) -> Option<TerminalAgent> {
        match self {
            PreferredAgent::Ask => None,
            PreferredAgent::Agent(agent) => Some(agent),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_roundtrip() {
        for agent in TerminalAgent::ALL {
            assert_eq!(TerminalAgent::from_tag(agent.tag()), Some(agent));
        }
        assert_eq!(TerminalAgent::from_tag("unknown"), None);
    }

    // `from_tag` is the session.json ingress whitelist for
    // the persisted `agent` field - hostile or malformed values (oversized,
    // control chars, near-misses) must all map to None so no pill renders.
    #[test]
    fn a_preferred_agent_round_trips_and_an_unknown_tag_is_no_answer() {
        assert_eq!(PreferredAgent::Ask.tag(), "ask");
        assert_eq!(
            PreferredAgent::from_tag("ask"),
            Some(PreferredAgent::Ask),
            "the deliberate choice to be asked has a value of its own"
        );
        assert_eq!(
            PreferredAgent::from_tag(PreferredAgent::Agent(TerminalAgent::Codex).tag()),
            Some(PreferredAgent::Agent(TerminalAgent::Codex))
        );
        // An agent this build does not know is not "never asked" either - the
        // restore path turns `None` into `Ask`, so a stored value that cannot
        // be honoured fails to the question rather than to somebody else's
        // default.
        assert_eq!(PreferredAgent::from_tag("some_future_cli"), None);
    }

    #[test]
    fn the_three_states_are_three() {
        // The middle one is what an `Option<TerminalAgent>` could not hold:
        // `None` is "never asked" and inherits, `Ask` is a person's answer.
        assert_eq!(PreferredAgent::Ask.agent(), None);
        assert_eq!(
            PreferredAgent::Agent(TerminalAgent::ClaudeCode).agent(),
            Some(TerminalAgent::ClaudeCode)
        );
    }

    #[test]
    fn from_tag_rejects_hostile_session_values() {
        assert_eq!(TerminalAgent::from_tag(""), None);
        assert_eq!(
            TerminalAgent::from_tag("Claude_Code"),
            None,
            "case-sensitive"
        );
        assert_eq!(TerminalAgent::from_tag("claude_code "), None, "no trim");
        assert_eq!(TerminalAgent::from_tag("claude_code\u{202e}"), None);
        assert_eq!(TerminalAgent::from_tag("codex\n"), None);
        assert_eq!(TerminalAgent::from_tag(&"x".repeat(10_000)), None);
    }

    #[test]
    fn binary_roundtrip_via_from_binary() {
        // The scan's comm match resolves back to the agent.
        for agent in TerminalAgent::ALL {
            assert_eq!(TerminalAgent::from_binary(agent.binary()), Some(agent));
        }
        assert_eq!(TerminalAgent::from_binary("bash"), None);
        assert_eq!(TerminalAgent::from_binary("claude-code-cli"), None);
    }

    #[test]
    fn binary_is_launch_command_leading_token() {
        // The PATH probe (`binary`) must match the actual executable the
        // launcher runs, or default visibility detects the wrong binary.
        let cfg = SplitlaneConfig::default();
        for agent in TerminalAgent::ALL {
            let command = agent.command(&cfg);
            let leading = command.split_whitespace().next().unwrap_or_default();
            assert_eq!(
                leading,
                agent.binary(),
                "{} binary must match its launch command's leading token",
                agent.display_name()
            );
        }
    }

    #[test]
    fn explicit_visibility_overrides_install_detection() {
        // `Some(true)`/`Some(false)` win over PATH detection, so the result
        // is deterministic on any machine (and never touches the filesystem
        // here - the `unwrap_or_else` install probe is short-circuited).
        let shown = SplitlaneConfig {
            gemini_button_visible: Some(true),
            ..Default::default()
        };
        assert!(TerminalAgent::Gemini.is_visible(&shown));

        let hidden = SplitlaneConfig {
            gemini_button_visible: Some(false),
            ..Default::default()
        };
        assert!(!TerminalAgent::Gemini.is_visible(&hidden));
    }

    /// Settings writes visibility through `visibility_key`, so every key it
    /// names has to be the one `is_visible` reads back - otherwise a switch
    /// flips and nothing changes.
    #[test]
    fn the_visibility_key_is_the_key_is_visible_reads() {
        for agent in TerminalAgent::ALL {
            for state in [true, false] {
                let document = format!("{{\"{}\": {state}}}", agent.visibility_key());
                let config: SplitlaneConfig = serde_json::from_str(&document)
                    .unwrap_or_else(|e| panic!("{} config: {e}", agent.display_name()));
                assert_eq!(
                    agent.is_visible(&config),
                    state,
                    "{} reads back {}",
                    agent.display_name(),
                    agent.visibility_key()
                );
            }
        }
    }

    #[test]
    fn icon_paths_are_embedded_assets() {
        // Every icon must live under an embedded asset root (`icons/` or
        // `agents/`) or the tab-bar `svg()` silently renders nothing.
        for agent in TerminalAgent::ALL {
            let p = agent.icon_path();
            assert!(
                p.starts_with("icons/") || p.starts_with("agents/"),
                "{} icon path `{p}` is not under an embedded asset root",
                agent.display_name()
            );
        }
    }

    #[test]
    fn claude_bypass_flag_toggles_command() {
        let off = SplitlaneConfig {
            claude_code_bypass_permissions: Some(false),
            ..Default::default()
        };
        assert_eq!(TerminalAgent::ClaudeCode.command(&off), "claude");
        let on = SplitlaneConfig {
            claude_code_bypass_permissions: Some(true),
            ..Default::default()
        };
        assert_eq!(
            TerminalAgent::ClaudeCode.command(&on),
            "claude --permission-mode bypassPermissions"
        );
    }

    #[test]
    fn non_claude_agents_ignore_bypass() {
        let config = SplitlaneConfig {
            claude_code_bypass_permissions: Some(true),
            ..Default::default()
        };
        assert_eq!(TerminalAgent::Codex.command(&config), "codex");
        assert_eq!(TerminalAgent::Pi.command(&config), "pi");
        assert_eq!(TerminalAgent::Hermes.command(&config), "hermes");
    }

    #[test]
    fn launch_spec_keeps_program_and_args_structured_until_render() {
        let cfg = SplitlaneConfig {
            claude_code_bypass_permissions: Some(true),
            ..Default::default()
        };

        let spec = TerminalAgent::ClaudeCode.launch_spec(&cfg);

        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, vec!["--permission-mode", "bypassPermissions"]);
        assert_eq!(
            spec.render_shell_command(),
            "claude --permission-mode bypassPermissions"
        );
    }

    #[test]
    fn launch_spec_plain_token_guard_matches_agent_command_surface() {
        for agent in TerminalAgent::ALL {
            assert!(
                is_plain_shell_token(agent.binary()),
                "{} binary must stay a plain shell token",
                agent.display_name()
            );
            for arg in agent.command_args() {
                assert!(
                    is_plain_shell_token(arg),
                    "{} arg `{arg}` must stay a plain shell token",
                    agent.display_name()
                );
            }
        }
        assert!(is_plain_shell_token(SAMPLE_UUID));
        assert!(!is_plain_shell_token("two words"));
        assert!(!is_plain_shell_token("$(reboot)"));
    }

    const SAMPLE_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn only_claude_supports_forced_session_id() {
        assert!(TerminalAgent::ClaudeCode.supports_forced_session_id());
        for agent in TerminalAgent::ALL
            .into_iter()
            .filter(|a| *a != TerminalAgent::ClaudeCode)
        {
            assert!(
                !agent.supports_forced_session_id(),
                "{} must not force a session id",
                agent.display_name()
            );
        }
    }

    #[test]
    fn session_agent_maps_only_readable_stores() {
        use crate::agent_sessions::SessionAgent;
        assert_eq!(
            TerminalAgent::ClaudeCode.session_agent(),
            Some(SessionAgent::Claude)
        );
        assert_eq!(
            TerminalAgent::Codex.session_agent(),
            Some(SessionAgent::Codex)
        );
        assert_eq!(
            TerminalAgent::OpenCode.session_agent(),
            Some(SessionAgent::OpenCode)
        );
        assert_eq!(TerminalAgent::Pi.session_agent(), Some(SessionAgent::Pi));
        assert_eq!(
            TerminalAgent::Hermes.session_agent(),
            Some(SessionAgent::Hermes)
        );
        assert_eq!(
            TerminalAgent::Grok.session_agent(),
            Some(SessionAgent::Grok)
        );
        assert_eq!(
            TerminalAgent::Cursor.session_agent(),
            Some(SessionAgent::Cursor)
        );
        assert_eq!(
            TerminalAgent::Gemini.session_agent(),
            Some(SessionAgent::Gemini)
        );
        assert_eq!(
            TerminalAgent::Kiro.session_agent(),
            Some(SessionAgent::Kiro)
        );
        assert_eq!(TerminalAgent::Amp.session_agent(), None);
        assert_eq!(TerminalAgent::Antigravity.session_agent(), None);
        assert_eq!(TerminalAgent::Copilot.session_agent(), None);
        assert_eq!(TerminalAgent::CodeBuddy.session_agent(), None);
        assert_eq!(TerminalAgent::Factory.session_agent(), None);
        assert_eq!(TerminalAgent::Qoder.session_agent(), None);
        assert_eq!(TerminalAgent::Openclaw.session_agent(), None);
    }

    #[test]
    fn claude_session_id_is_injected_after_binary() {
        let cfg = SplitlaneConfig::default();
        let cmd =
            TerminalAgent::ClaudeCode.command_with_session(&cfg, SessionBinding::Mint(SAMPLE_UUID));
        assert_eq!(cmd, format!("claude --session-id {SAMPLE_UUID}"));
        // Leading token stays `claude` (the PATH-probe invariant).
        assert_eq!(cmd.split_whitespace().next(), Some("claude"));
    }

    #[test]
    fn claude_resume_reattaches_instead_of_minting() {
        // Regression: reopening a thread after a restart used to re-send
        // `--session-id <existing uuid>`, which Claude Code rejects with
        // `Error: Session ID <uuid> is already in use.` An id that already
        // has a session file must go out as `--resume`.
        let cfg = SplitlaneConfig::default();
        let cmd = TerminalAgent::ClaudeCode
            .command_with_session(&cfg, SessionBinding::Resume(SAMPLE_UUID));
        assert_eq!(cmd, format!("claude --resume {SAMPLE_UUID}"));
        assert!(
            !cmd.contains("--session-id"),
            "resume must never re-mint the id"
        );
        assert_eq!(cmd.split_whitespace().next(), Some("claude"));
    }

    #[test]
    fn claude_session_id_composes_with_bypass() {
        let cfg = SplitlaneConfig {
            claude_code_bypass_permissions: Some(true),
            ..Default::default()
        };
        let mint =
            TerminalAgent::ClaudeCode.command_with_session(&cfg, SessionBinding::Mint(SAMPLE_UUID));
        assert_eq!(
            mint,
            format!("claude --session-id {SAMPLE_UUID} --permission-mode bypassPermissions")
        );
        let resume = TerminalAgent::ClaudeCode
            .command_with_session(&cfg, SessionBinding::Resume(SAMPLE_UUID));
        assert_eq!(
            resume,
            format!("claude --resume {SAMPLE_UUID} --permission-mode bypassPermissions")
        );
    }

    #[test]
    fn invalid_session_id_is_not_injected() {
        // Flag-shaped / shell-meta ids fail the allow-list, so a tampered
        // session.json can never smuggle a second argument into the launch -
        // on either arm.
        let cfg = SplitlaneConfig::default();
        for hostile in [
            "--dangerously-skip-permissions",
            "-x",
            "x; rm -rf ~",
            "$(reboot)",
            // A bare space would split into a second, positional argument
            // even without any shell metacharacter.
            "abc def",
        ] {
            assert_eq!(
                TerminalAgent::ClaudeCode.command_with_session(&cfg, SessionBinding::Mint(hostile)),
                "claude",
                "hostile id {hostile:?} must be dropped when minting"
            );
            assert_eq!(
                TerminalAgent::ClaudeCode
                    .command_with_session(&cfg, SessionBinding::Resume(hostile)),
                "claude",
                "hostile id {hostile:?} must be dropped when resuming"
            );
        }
    }

    #[test]
    fn session_binding_resolves_unbound_without_an_id() {
        assert_eq!(
            SessionBinding::resolve(None, "/tmp/splitlane-does-not-exist"),
            SessionBinding::Unbound
        );
    }

    #[test]
    fn session_binding_mints_when_no_session_file_exists() {
        // No project dir for this cwd, so the id cannot have a session file
        // and the first launch must mint it.
        assert_eq!(
            SessionBinding::resolve(Some(SAMPLE_UUID), "/tmp/splitlane-no-such-project-dir"),
            SessionBinding::Mint(SAMPLE_UUID)
        );
    }

    #[test]
    fn bare_commands_preserve_multi_token_agent_commands() {
        let cfg = SplitlaneConfig::default();
        assert_eq!(TerminalAgent::Kiro.command(&cfg), "kiro-cli chat");
        assert_eq!(TerminalAgent::Openclaw.command(&cfg), "openclaw tui");
    }

    #[test]
    fn non_claude_ignores_forced_session_id() {
        let cfg = SplitlaneConfig::default();
        for binding in [
            SessionBinding::Mint(SAMPLE_UUID),
            SessionBinding::Resume(SAMPLE_UUID),
        ] {
            assert_eq!(
                TerminalAgent::Codex.command_with_session(&cfg, binding),
                "codex"
            );
            assert_eq!(
                TerminalAgent::OpenCode.command_with_session(&cfg, binding),
                "opencode"
            );
        }
    }

    #[test]
    fn claude_without_session_id_is_bare_command() {
        let cfg = SplitlaneConfig::default();
        assert_eq!(
            TerminalAgent::ClaudeCode.command_with_session(&cfg, SessionBinding::Unbound),
            "claude"
        );
    }
}
