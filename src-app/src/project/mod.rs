// This module was the foundation of the Agents view domain model. Many
// items here were deliberately "unused" until follow-up work activated them:
// - ID counters + `bump_id_counters_to`: the restore path.
// - `project_from_session` / `thread_from_session` / `agent_kind_from_str`:
//   restore from session.json.
// - `Thread::new` / `Project::new` constructors: sidebar
//   create-new affordances.
// - `ThreadStatus` non-`Idle` variants + `Thread::status`: the
//   streaming state machine.
//
// The pieces only take meaning when activated end-to-end; the foundation
// shipped first. The allow is module-scoped (not per-item) so the
// review surface stays one comment-line, not 12 attributes.
#![allow(dead_code)]

//! Runtime domain model for the Agents view.
//!
//! Mirrors the existing [`crate::workspace::Workspace`] shape:
//! - Ids from the process-wide monotonic counter (`workspace::next_id`)
//! - A small struct holding the live state
//! - Conversion to/from the matching [`splitlane_config::schema`]
//!   "session" struct so save/restore round-trips cleanly
//!
//! The split between runtime types (here) and session types (in
//! `splitlane-config`) is intentional: session types are pure data and
//! belong to a leaf crate so the schema can evolve without dragging the
//! whole app crate into the same compile unit. Runtime types carry the
//! richer enums and conversion helpers.

use splitlane_acp::AgentKind;
use splitlane_config::schema::{
    AgentSurface, AgentsTargetSession, ProjectSurface, SurfaceKind, SurfacePlacement,
};

use crate::workspace::Workspace;

/// Restore caps for `session.json`. The file-size cap protects memory, but a
/// compact session can still encode thousands of containers/chats that fan out
/// into git probes, rail rows, and terminal-cache work after boot. The
/// container cap itself is [`crate::workspace::MAX_WORKSPACES`] - one list,
/// one cap.
pub const MAX_RESTORED_THREADS_PER_PROJECT: usize = 256;
pub const MAX_RESTORED_CHATS: usize = 512;
pub const MAX_RESTORED_TOTAL_THREADS: usize = 2048;

/// Projects and threads draw from the process-wide counter
/// ([`crate::workspace::next_id`]) - the same one workspaces use. Ids are
/// unique across every container and surface, which is what lets a PTY's
/// `SPLITLANE_WORKSPACE_ID` be the raw id with no namespace offset.
pub fn next_thread_id() -> u64 {
    crate::workspace::next_id()
}

/// The highest id `session.json` carries - over containers, their surfaces,
/// and the free chats alike.
///
/// The restore path bumps the shared counter past this before anything is
/// minted, so a fresh id can never land on a restored one. It reads the FILE,
/// not the restored objects: a container this build drops at a cap, or a
/// surface it cannot parse, still occupies its id in the file next to the one
/// a new container would otherwise be handed.
///
/// Free `chats` are `Thread`s from the same counter, so they MUST be folded in.
pub fn max_persisted_id(session: &splitlane_config::schema::SessionState) -> u64 {
    session
        .projects
        .iter()
        .map(|container| container.id)
        .chain(
            session
                .projects
                .iter()
                .flat_map(|container| container.surfaces.iter().map(|s| s.id)),
        )
        .chain(session.chats.iter().map(|s| s.id))
        .max()
        .unwrap_or(0)
}

/// What the content area is showing - one surface, named by where it lives.
///
/// This is the selection the rail draws a highlight on, and it is what
/// replaced [`splitlane_config::schema::AppMode`]: the app no longer has a mode
/// deciding which screen renders, it has a selected surface. Every arm is a
/// positional index into the live `workspaces` / `chats` vectors, kept in
/// range by the data-plane ops (`select_thread`, `remove_thread`,
/// `close_workspace_at`). The PTY warm-resume
/// cache is keyed by the stable `Thread::id`, not by this target, so
/// navigating between surfaces never tears down a running shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsTarget {
    /// The slot tree of `workspaces[ws_idx]` - its shell and markdown
    /// surfaces, laid out by the split tree.
    Panes { ws_idx: usize },
    /// A parked agent surface: `workspaces[ws_idx].threads[thread_idx]`.
    Thread { ws_idx: usize, thread_idx: usize },
    /// The git diff of `workspaces[ws_idx]`.
    Diff { ws_idx: usize },
}

impl AgentsTarget {
    /// The container this surface belongs to. Every surface has one: free
    /// chats - the last surfaces that belonged to no container - are agent
    /// surfaces of the home directory's container now.
    pub fn ws_idx(self) -> usize {
        match self {
            AgentsTarget::Panes { ws_idx }
            | AgentsTarget::Thread { ws_idx, .. }
            | AgentsTarget::Diff { ws_idx } => ws_idx,
        }
    }

    /// The same surface, re-anchored on `ws_idx`.
    pub fn with_ws_idx(self, ws_idx: usize) -> Self {
        match self {
            AgentsTarget::Panes { .. } => AgentsTarget::Panes { ws_idx },
            AgentsTarget::Thread { thread_idx, .. } => AgentsTarget::Thread { ws_idx, thread_idx },
            AgentsTarget::Diff { .. } => AgentsTarget::Diff { ws_idx },
        }
    }
}

/// Convert a stable persisted target back to runtime indices.
pub fn agents_target_from_session(
    target: &AgentsTargetSession,
    containers: &[Workspace],
) -> Option<AgentsTarget> {
    let resolved = resolve_agents_target(target, containers);
    if resolved.is_none() {
        // Losing the selection is silent otherwise: the app comes up on the
        // default surface and nothing says the saved one could not be found.
        log::warn!("session restore: saved selection {target:?} no longer resolves");
    }
    resolved
}

fn resolve_agents_target(
    target: &AgentsTargetSession,
    containers: &[Workspace],
) -> Option<AgentsTarget> {
    match *target {
        AgentsTargetSession::Thread {
            project_id,
            thread_id,
        } => {
            let ws_idx = containers
                .iter()
                .position(|container| container.id == project_id)?;
            let thread_idx = containers[ws_idx]
                .threads
                .iter()
                .position(|thread| thread.id == thread_id)?;
            Some(AgentsTarget::Thread { ws_idx, thread_idx })
        }
        // A free chat written by an older build. Chats are agent surfaces of
        // the home directory's container now, and the fold happens before this
        // runs, so the id is found by the same search every other surface uses.
        AgentsTargetSession::Chat { thread_id } => find_surface(containers, thread_id),
        AgentsTargetSession::Panes { project_id } => containers
            .iter()
            .position(|container| container.id == project_id)
            .map(|ws_idx| AgentsTarget::Panes { ws_idx }),
        AgentsTargetSession::Diff { project_id } => containers
            .iter()
            .position(|container| container.id == project_id)
            .map(|ws_idx| AgentsTarget::Diff { ws_idx }),
        // A target this build cannot read restores as no selection - the rail
        // is one click away, and a wrong surface would be worse.
        AgentsTargetSession::Unknown => None,
    }
}

/// Locate an agent surface by its stable id, anywhere in the container list.
pub fn find_surface(containers: &[Workspace], thread_id: u64) -> Option<AgentsTarget> {
    containers
        .iter()
        .enumerate()
        .find_map(|(ws_idx, container)| {
            container
                .threads
                .iter()
                .position(|thread| thread.id == thread_id)
                .map(|thread_idx| AgentsTarget::Thread { ws_idx, thread_idx })
        })
}

/// Per-thread state machine for the Agents sidebar. It stays deliberately
/// small: the row renders compact visual indicators only, never status text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadStatus {
    #[default]
    Idle,
    /// The launch command has been issued and nothing has come back yet
    /// The fifth word, and the only one that is **not** a detector
    /// state: it is true by construction rather than by detection, which is
    /// what makes it honest for all sixteen agents.
    ///
    /// It exists because the header lied for two seconds. Opening a session
    /// showed `idle` while `claude --resume <uuid>` sat on screen waiting for
    /// the CLI to paint - and the alternative, a loader over the pane body,
    /// would have covered the only two seconds that explain a failure.
    Starting,
    Thinking,
    WaitingForInput,
    Failed,
}

impl ThreadStatus {
    /// Derive the Agents sidebar state from the hook-backed agent lifecycle.
    ///
    /// The Agents rail intentionally stays compact: active states render as
    /// visual indicators, not status text.
    pub fn from_agent_state(state: crate::ai_types::AgentState) -> Self {
        match state {
            crate::ai_types::AgentState::Thinking | crate::ai_types::AgentState::Stalled => {
                ThreadStatus::Thinking
            }
            crate::ai_types::AgentState::WaitingForInput => ThreadStatus::WaitingForInput,
            crate::ai_types::AgentState::Finished => ThreadStatus::Idle,
            crate::ai_types::AgentState::Errored => ThreadStatus::Failed,
        }
    }
}

/// What kind of surface a thread row drives in the main area. The
/// Agents view is terminal-only since the in-app ACP chat was removed,
/// so every live thread renders a `Terminal` PTY surface (launching the
/// thread's [`crate::agent_launcher::TerminalAgent`] CLI). `Agent` is a
/// legacy variant retained only so a pre-removal `session.json` (chat
/// threads) still deserializes; those rows are routed through the same
/// terminal path at render time and relaunch their original agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadKind {
    /// Legacy chat thread restored from an older session. Rendered as a
    /// terminal (relaunching the stored `agent`); never created anew.
    #[default]
    Agent,
    /// A PTY surface in the thread's cwd, optionally auto-launching a
    /// CLI agent (`terminal_agent`).
    Terminal,
}

/// One persistent thread row in the Agents sidebar. A Terminal Thread:
/// the PTY is the source of truth and only the sidebar metadata
/// round-trips through session.json.
/// The read/unread mark on a surface whose run ended out of sight, and **how
/// far the reader has got with it**.
///
/// Two levels, because the scale needs two and they are cleared by different
/// things. A fresh mark is *not acknowledged*: the person has not seen even
/// the list, and the chip says so at the top of its scale. Opening Activity
/// **lowers** it to the quiet mark - "known, not collected" - and only the
/// surface actually coming up in a pane takes it away.
///
/// **The sticky one is the quiet mark, not the shout**, which is the whole
/// correction to an earlier design where opening the popover cleared
/// everything. It closes two failures at once: a ritual walk of the panes to
/// buy silence, which would have devalued the word "read" a second way, and
/// desensitisation - a chip that is always hot is not a chip.
#[derive(Debug, Clone, Copy)]
pub struct FinishedMark {
    /// When the run ended. **Nothing decays on it** - see the field's own note.
    pub at: std::time::Instant,
    /// Whether the reader has seen this in the Activity list.
    pub acknowledged: bool,
}

impl FinishedMark {
    /// A run that has just ended out of sight: nobody has seen even the list.
    pub fn unacknowledged() -> Self {
        Self {
            at: std::time::Instant::now(),
            acknowledged: false,
        }
    }

    /// How long ago the run ended.
    pub fn elapsed(&self) -> std::time::Duration {
        self.at.elapsed()
    }
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: u64,
    pub title: String,
    /// **Do not gate anything on this for a `Terminal` thread.** It is a
    /// placeholder: `Thread::new_terminal` writes `ClaudeCode` into it whatever
    /// CLI the surface actually launches, and the launcher is
    /// [`Self::terminal_agent`].
    ///
    /// `ThreadKind`'s own comment says the Terminal dispatch never consults it,
    /// and that was read as a licence rather than as a warning: "Copy the last
    /// answer" gated on `thread.agent == ClaudeCode`, which is true of every
    /// terminal surface, so the item appeared on Codex panes and then claimed
    /// there was no answer to copy - a statement about somebody else's session.
    /// No grep finds that, because the code reads correct; it was found by
    /// looking at the screen (26 August).
    ///
    /// The question worth asking is almost always "can this build read that
    /// agent's transcript", and it has one home:
    /// `claude_sessions::transcript_path`.
    pub agent: AgentKind,
    pub kind: ThreadKind,
    pub status: ThreadStatus,
    pub cwd: String,
    pub created_at: u64,
    pub model: Option<String>,
    pub mode: Option<String>,
    /// Vestigial foreign key from the removed `splitlane-threads` chat
    /// store. No longer read or written (terminal threads have no SQL
    /// rows); kept on the struct only so a pre-removal `session.json`
    /// round-trips its `store_id` field without data loss.
    pub store_id: Option<String>,
    /// Which CLI coding agent a [`ThreadKind::Terminal`] thread launches
    /// on first PTY mount. `None` for a bare shell (the plain
    /// "New terminal thread" affordance + legacy Agent rows). Always
    /// `None` for `ThreadKind::Agent`.
    pub terminal_agent: Option<crate::agent_launcher::TerminalAgent>,
    /// Whether the user
    /// pinned this thread. Pinned threads (project threads and free chats
    /// alike) are surfaced in the rail's PINNED section. Round-trips through
    /// [`ThreadSession::pinned`]; a restored thread without the flag is
    /// `false`.
    pub pinned: bool,
    /// PID of the CLI agent currently driving [`Self::status`], reported
    /// by the `ai.*` hook frames. Transient (never persisted - a restored
    /// thread is always `Idle`): only consumed by the stale-PID sweep so a
    /// killed agent can't leave the sidebar spinner running forever.
    /// `None` for legacy hook shims that omit `pid`; the sweep then keeps
    /// the state conservatively (same policy as workspace sessions).
    pub agent_pid: Option<u32>,
    /// Opaque OS process start fingerprint for [`Self::agent_pid`].
    /// Transient and best-effort: when present, stale sweeping can detect PID
    /// reuse instead of treating a recycled PID as the original live agent.
    pub agent_proc_start: Option<u64>,
    /// Forced agent session UUID for a Claude Terminal Thread. Generated
    /// at creation for agents that support `--session-id`
    /// ([`crate::agent_launcher::TerminalAgent::supports_forced_session_id`])
    /// and injected into the launch command so the live thread maps 1:1 to
    /// its on-disk session file. Round-trips through
    /// [`ThreadSession::session_id`]; `None` for bare shells and non-Claude
    /// agents.
    pub session_id: Option<String>,
    /// Whether the user manually renamed this row. When `true`,
    /// [`SplitlaneApp::handle_terminal_thread_title_changed`] and the
    /// `ai-title` backfill leave the title untouched so a deliberate name
    /// survives agent activity. Round-trips through
    /// [`ThreadSession::title_user_set`].
    pub title_user_set: bool,
    /// Whether the running process has reported a title of its own for this
    /// surface - an OSC 0/2 from the CLI in the pane.
    ///
    /// **Runtime only, deliberately not persisted.** It is a claim about a
    /// process that is on screen now; a restored surface has no process and
    /// must be free to take the `ai-title` summary as its name until its CLI
    /// paints one.
    ///
    /// It exists because two automatic sources wrote one field. The OSC title
    /// says what the agent is doing *now*; the `ai-title` backfill reads the
    /// session's stored summary at every turn end. Both landed on
    /// [`Self::title`] and the last one to arrive won, so the rail drifted to
    /// the summary while the pane header - which reads the live title - went
    /// on showing the other name. One surface, two names, and neither of them
    /// wrong on its own. The process wins while it is speaking: it is the more
    /// current of the two, and it is the one the header already shows.
    pub title_from_process: bool,
    /// How fast this surface has been spending, and over how long that was
    /// measured. Written by the slow tail probe beside the model, from the
    /// agent's own transcript.
    ///
    /// **Runtime only and deliberately not persisted**, like the process title
    /// beside it: a rate is a statement about the last ten minutes, and a rate
    /// restored from disk would be a statement about ten minutes that ended
    /// whenever the app was last closed.
    ///
    /// `None` is "not measured" - an unpriced model, a session with no reply
    /// yet, a span too short to divide by, or one of the fifteen agents whose
    /// transcript this build cannot price - and never "spending nothing".
    pub spend: Option<crate::agent_state::SpendRate>,
    /// How many tokens the newest turn carried into the model. A count, and
    /// deliberately not a share: nothing on disk states the window it would be
    /// a share of. Runtime only, like the rate beside it.
    pub context_tokens: Option<u64>,
    /// The size this session's own automatic compaction fired at - the meter's
    /// denominator, and the only one that is measured rather than assumed.
    ///
    /// **Kept once seen.** The record that carries it scrolls out of the tail
    /// the probe reads as soon as the next segment grows, so the value is
    /// caught in passing and held; a later automatic compaction overwrites it,
    /// which is how a changed plan corrects itself without anybody being asked.
    /// Runtime only, like the two facts beside it.
    pub context_ceiling: Option<u64>,
    /// The newest compaction this app has seen in the surface's transcript.
    ///
    /// Carries the ring's whole content: whether it is still on
    /// (`turn_since == false`), and the words behind it - when, and from what
    /// to what.
    pub last_compaction: Option<crate::claude_sessions::CompactionSeen>,
    /// How many compactions this app has **seen**.
    ///
    /// A floor rather than a total: a session the app joined halfway through
    /// had earlier ones nobody here watched. It is exact in the ordinary case,
    /// where the app started the session it is watching.
    pub compactions_seen: u32,
    /// When this surface's run ended without anybody being able to see it, or
    /// `None` once it has been looked at.
    ///
    /// **A different axis from [`Self::status`], not a sixth word for it**
    /// The five status words all answer "what is this session
    /// doing"; whether *you* have looked since it finished is the read/unread
    /// axis - orthogonal to all five, self-clearing by being read, and so
    /// addable without touching the vocabulary or spending the accent. The dot
    /// goes to `idle` immediately and honestly; the row additionally reads as
    /// unseen until it is opened.
    ///
    /// The instant is here for the popover, which says `finished 4m ago` beside
    /// the way a waiting row says `waiting 21m`. **Nothing decays on it** - a
    /// mark that faded on a timer would be an event again, lost on anybody who
    /// looked away for longer than the fuse, which is the toast's defect with a
    /// longer wick.
    ///
    /// Runtime only, and deliberately not persisted: a relaunch is a fresh
    /// look, and news that survives a quit is not news. (`Instant` has no
    /// serialisation either, and that is a symptom of the same fact rather than
    /// the reason.)
    pub finished_unseen: Option<FinishedMark>,
    /// When the state detector's most recent reading for this thread was
    /// **taken**, or `None` when it had nothing to say. Also who owns
    /// [`Self::status`].
    ///
    /// The two sources of a thread's state disagree by construction. A hook
    /// frame reports the moment it fires - `ai.tool_use` says `Thinking` and
    /// then nothing more, so a call held open waiting for a person keeps the
    /// last thing the hook said. The detector reads the same turn out of the
    /// CLI's own transcript and calls it `WaitingForInput`. Left to write the
    /// same field, they would take turns and the dot would blink.
    ///
    /// So the detector is the source and the hook is an accelerator: while this
    /// is `Some` the hook stops writing `status` and instead asks for an
    /// immediate re-read (`app::agent_state_pass`). If the hook contour ever
    /// goes away the system degrades to "slow", not to "wrong" - which is the
    /// trade this whole road was chosen for.
    ///
    /// It is set and cleared on **every** pass rather than latched. A thread
    /// whose transcript stops being readable hands ownership straight back to
    /// the hook instead of freezing on its last reading - and the detector
    /// **withdraws** that reading at the same time, because a claim that
    /// somebody is being kept waiting has to be renewed by evidence or it is no
    /// longer a claim about anything.
    ///
    /// It is a timestamp rather than a flag because two readers write it: the
    /// periodic pass and the hook's out-of-turn re-read, which run
    /// concurrently and can finish out of order. Comparing when each reading
    /// was *taken* is what stops an older one landing on top of a newer.
    /// Transient: never persisted, and `None` on every restored thread.
    pub detector_read_at: Option<std::time::Instant>,
    /// Whether any `ai.*` hook frame has ever spoken for this surface.
    ///
    /// Transient and never persisted: it is a fact about this process's
    /// lifetime, and a restored surface has heard nothing yet.
    ///
    /// It exists to rank the **third** source of state below the other two
    /// without either of them having to know it is there. The PTY-flow signal
    /// (`app::agent_state_pass`) says "working" from output arriving, which is
    /// available for all sixteen agents and costs no vendor anything - but it
    /// is the weakest thing anyone can say, and it must never displace an agent
    /// that reports itself. The detector defends itself already
    /// ([`Self::detector_read_at`]); the hook could not, because it writes
    /// `Idle` at the end of every turn and the flow signal would immediately
    /// contradict it.
    ///
    /// So: one frame from a hook, and this surface is the hook's for good.
    /// Deliberately **not** a per-agent list of who emits frames - the shim
    /// decides that per binary, and a second copy of that list in the app is a
    /// copy that drifts. The surface answers for itself.
    pub hook_has_spoken: bool,
}

impl Thread {
    /// Create a fresh thread with an auto-allocated ID and `Idle` status.
    pub fn new(title: impl Into<String>, agent: AgentKind, cwd: impl Into<String>) -> Self {
        Self {
            id: next_thread_id(),
            title: title.into(),
            agent,
            kind: ThreadKind::Agent,
            status: ThreadStatus::Idle,
            cwd: cwd.into(),
            created_at: now_unix_millis(),
            model: None,
            mode: None,
            store_id: None,
            terminal_agent: None,
            pinned: false,
            agent_pid: None,
            agent_proc_start: None,
            session_id: None,
            title_user_set: false,
            title_from_process: false,
            spend: None,
            context_tokens: None,
            context_ceiling: None,
            last_compaction: None,
            compactions_seen: 0,
            finished_unseen: None,
            detector_read_at: None,
            hook_has_spoken: false,
        }
    }

    /// Create a fresh Terminal Thread bound to `terminal_agent` (the CLI
    /// auto-launched on first PTY mount, or `None` for a bare shell).
    /// The `agent` slot is filled with a placeholder (`ClaudeCode`) that
    /// the Terminal dispatch never consults - see [`ThreadKind`] for the
    /// rationale.
    pub fn new_terminal(
        title: impl Into<String>,
        cwd: impl Into<String>,
        terminal_agent: Option<crate::agent_launcher::TerminalAgent>,
    ) -> Self {
        Self::new_terminal_inner(title, cwd, terminal_agent, None)
    }

    /// [`Self::new_terminal`] bound to a session the CLI already owns, rather
    /// than a freshly minted one. Backs "open a session from history": the
    /// thread adopts the picked `session_id`, so its first PTY mount resolves
    /// to [`crate::agent_launcher::SessionBinding::Resume`] and the
    /// conversation comes back instead of starting empty.
    ///
    /// The id is dropped when the agent does not support a forced session id,
    /// so a non-Claude thread can never be handed one it cannot use.
    pub fn new_terminal_for_session(
        title: impl Into<String>,
        cwd: impl Into<String>,
        terminal_agent: Option<crate::agent_launcher::TerminalAgent>,
        session_id: impl Into<String>,
    ) -> Self {
        Self::new_terminal_inner(title, cwd, terminal_agent, Some(session_id.into()))
    }

    fn new_terminal_inner(
        title: impl Into<String>,
        cwd: impl Into<String>,
        terminal_agent: Option<crate::agent_launcher::TerminalAgent>,
        bound_session: Option<String>,
    ) -> Self {
        // Adopt the caller's session id when resuming from history, otherwise
        // mint a fresh UUID for agents that accept `--session-id` (Claude only
        // today). Either way the live thread maps 1:1 to a known on-disk
        // session file, so the sidebar can adopt its `ai-title` (parity with
        // `/resume`) even when several threads share a cwd.
        let session_id = terminal_agent
            .filter(|a| a.supports_forced_session_id())
            .map(|_| bound_session.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()));
        Self {
            id: next_thread_id(),
            title: title.into(),
            agent: AgentKind::ClaudeCode,
            kind: ThreadKind::Terminal,
            status: ThreadStatus::Idle,
            cwd: cwd.into(),
            created_at: now_unix_millis(),
            model: None,
            mode: None,
            store_id: None,
            terminal_agent,
            pinned: false,
            agent_pid: None,
            agent_proc_start: None,
            session_id,
            title_user_set: false,
            title_from_process: false,
            spend: None,
            context_tokens: None,
            context_ceiling: None,
            last_compaction: None,
            compactions_seen: 0,
            finished_unseen: None,
            detector_read_at: None,
            hook_has_spoken: false,
        }
    }
}

/// Convert an in-memory [`Thread`] to its persisted shape: an `agent`
/// surface. The runtime `status` is intentionally not persisted -- every
/// thread restores as `Idle`; live state is rebuilt from hook/IPC events.
/// `slot` is the index of the layout leaf showing this surface, or `None` when
/// it is parked - derived from the live tree by the caller
/// ([`crate::app::agent_slots::agent_leaf_index`]), never stored on the thread.
pub fn thread_to_surface(t: &Thread, slot: Option<usize>) -> ProjectSurface {
    ProjectSurface {
        id: t.id,
        kind: SurfaceKind::Agent,
        // Where the surface is showing. The layout records the same fact, as
        // the tab that references this record's id, and that is what restore
        // reads - it also knows the tab's order and which tab is active, which
        // a leaf index cannot express. This field is the schema's own name for
        // the placement and is kept honest rather than left saying "parked"
        // about a surface that is plainly in a slot.
        placement: match slot {
            Some(index) => SurfacePlacement::Slot { index },
            None => SurfacePlacement::Parked,
        },
        title: clean_sidebar_title(&t.title).unwrap_or_else(|| "Terminal".to_string()),
        cwd: t.cwd.clone(),
        created_at: t.created_at,
        agent: Some(AgentSurface {
            agent: agent_kind_to_str(t.agent).to_string(),
            model: t.model.clone(),
            mode: t.mode.clone(),
            // `store_id` is set by the create-thread side-effect
            // (after inserting the `threads.db` row). Restoring a thread
            // from session without a `store_id` is legal -- the cascade
            // delete checks for `Some` before calling the store.
            store_id: t.store_id.clone(),
            // None for the legacy Agent kind so pre-Terminal-Thread
            // sessions round-trip unchanged.
            thread_kind: match t.kind {
                ThreadKind::Agent => None,
                ThreadKind::Terminal => Some(THREAD_KIND_TAG_TERMINAL.to_string()),
            },
            terminal_agent: t.terminal_agent.map(|a| a.tag().to_string()),
            // Persist the pin flag so a restart restores the
            // PINNED section.
            pinned: t.pinned,
            // Persist the forced Claude session id so a restart relaunches the
            // SAME session (resume + append), and the manual-rename lock so a
            // deliberate title is never re-clobbered after restore.
            session_id: t.session_id.clone(),
            title_user_set: t.title_user_set,
        }),
    }
}

/// The container's diff surface as a persisted record. It carries no payload
/// beyond its identity: what a diff shows is recomputed from git on open, so
/// there is nothing about it worth writing down except that it is open.
pub fn diff_surface_record(id: u64, cwd: &str) -> ProjectSurface {
    ProjectSurface {
        id,
        kind: SurfaceKind::Diff,
        placement: SurfacePlacement::Parked,
        title: "Changes".to_string(),
        cwd: cwd.to_string(),
        created_at: 0,
        agent: None,
    }
}

/// The id of the container's diff surface, if its record carries one.
pub fn diff_surface_from_session(s: &splitlane_config::schema::ProjectSession) -> Option<u64> {
    s.surfaces
        .iter()
        .find(|surface| surface.kind == SurfaceKind::Diff)
        .map(|surface| surface.id)
}

/// Rebuild a container's parked agent surfaces from its persisted record,
/// consuming from the shared restore budget. Unknown `agent` strings drop the
/// surface rather than the container - a tag a later build writes must cost at
/// most one row.
pub fn threads_from_session(
    s: &splitlane_config::schema::ProjectSession,
    remaining_threads: &mut usize,
) -> Vec<Thread> {
    if s.surfaces.len() > MAX_RESTORED_THREADS_PER_PROJECT {
        log::warn!(
            "session restore: container `{}` has {} surface(s), capping at {MAX_RESTORED_THREADS_PER_PROJECT}",
            s.title,
            s.surfaces.len()
        );
    }
    let mut threads = Vec::new();
    for surface in s.surfaces.iter().take(MAX_RESTORED_THREADS_PER_PROJECT) {
        if *remaining_threads == 0 {
            break;
        }
        if let Some(thread) = thread_from_surface(surface) {
            threads.push(thread);
            *remaining_threads = remaining_threads.saturating_sub(1);
        }
    }
    threads
}

pub fn thread_from_surface_with_budget(
    s: &ProjectSurface,
    remaining_threads: &mut usize,
) -> Option<Thread> {
    if *remaining_threads == 0 {
        return None;
    }
    let thread = thread_from_surface(s)?;
    *remaining_threads = remaining_threads.saturating_sub(1);
    Some(thread)
}

/// Inverse of [`thread_to_surface`]. Returns `None` for a surface that is not
/// an agent (nothing else restores into a thread) and on an unknown agent tag
/// (forward-compat for a future v1.x where OpenCode lands). Terminal-kind rows
/// ignore the `agent` field entirely (it carries a placeholder on disk for
/// forward-compat with pre-Terminal-Thread readers, see
/// `THREAD_KIND_TAG_TERMINAL`).
pub fn thread_from_surface(s: &ProjectSurface) -> Option<Thread> {
    if s.kind != SurfaceKind::Agent {
        return None;
    }
    let payload = s.agent.as_ref()?;
    let kind = match payload.thread_kind.as_deref() {
        Some(THREAD_KIND_TAG_TERMINAL) => ThreadKind::Terminal,
        Some(_) | None => ThreadKind::Agent,
    };
    let agent = match kind {
        ThreadKind::Agent => agent_kind_from_str(&payload.agent)?,
        // Terminal threads never dispatch through the agent path; the
        // placeholder keeps the struct shape uniform.
        ThreadKind::Terminal => AgentKind::ClaudeCode,
    };
    // Strip leading spinner/bullet decoration that CLI agents may
    // have baked into the title when it was last persisted (Claude
    // Code's `✻`, Codex's braille spinner, generic `●`). Falls back
    // to the raw title if cleaning yields nothing meaningful so the
    // row never restores empty.
    let title = clean_sidebar_title(&s.title).unwrap_or_else(|| "Terminal".to_string());
    Some(Thread {
        last_compaction: None,
        compactions_seen: 0,
        context_ceiling: None,
        context_tokens: None,
        spend: None,
        id: s.id,
        title,
        agent,
        kind,
        status: ThreadStatus::default(),
        cwd: s.cwd.clone(),
        created_at: s.created_at,
        model: payload.model.clone(),
        mode: payload.mode.clone(),
        store_id: payload.store_id.clone(),
        terminal_agent: payload
            .terminal_agent
            .as_deref()
            .and_then(crate::agent_launcher::TerminalAgent::from_tag),
        // An older surface defaults `pinned = false` via
        // `#[serde(default)]`, so this restores cleanly.
        pinned: payload.pinned,
        agent_pid: None,
        agent_proc_start: None,
        // Re-gate the restored session id through the same allow-list the
        // PTY-injection path uses: a tampered session.json must never
        // smuggle a flag-shaped value into `claude --session-id`. An
        // invalid id collapses to `None` - the thread relaunches as a fresh
        // session rather than refusing to open.
        session_id: payload
            .session_id
            .clone()
            .filter(|id| crate::agent_sessions::is_valid_session_id(id)),
        title_user_set: payload.title_user_set,
        title_from_process: false,
        finished_unseen: None,
        detector_read_at: None,
        hook_has_spoken: false,
    })
}

/// On-disk discriminant for [`ThreadKind::Terminal`] in
/// [`AgentSurface::thread_kind`]. Lives in its own constant so the round-trip
/// helpers and the affordance handlers agree on the literal.
pub const THREAD_KIND_TAG_TERMINAL: &str = "terminal";

/// Strip leading decoration glyphs and invisible characters that CLI
/// agents (Claude Code, Codex, OpenCode, Pi, Amp) bake into their
/// session / OSC titles to indicate status. Without this:
/// - During response: "● Project overview" sits in the sidebar with
///   a literal dot in front of the label.
/// - After response: a completion glyph (`✓`, `⚡`, …) or a
///   zero-width character (`U+200B`, `U+FEFF`, …) takes its place
///   and shows as a phantom margin -- `trim()` doesn't strip these
///   because they aren't whitespace per the Unicode standard, yet
///   most fonts render them with non-zero advance width.
///
/// Implementation strategy: whitelist what *can* legitimately lead a
/// human-written title (letters, digits, common opening punctuation)
/// and strip everything else from the front in one pass. That covers
/// the entire CLI-status-decoration family in a future-proof way --
/// new spinner glyphs or completion icons get caught without code
/// changes. Trailing whitespace is also normalized.
///
/// Returns `None` when nothing meaningful remains after stripping
/// (the caller treats that the same as an empty title -- the row
/// keeps its previous label rather than flashing blank).
const MAX_SIDEBAR_TITLE_CHARS: usize = 240;

pub fn clean_sidebar_title(raw: &str) -> Option<String> {
    let normalized: String = raw
        .chars()
        .map(|c| {
            if is_title_invisible_or_control(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let stripped = normalized
        .trim_start_matches(|c: char| !is_title_meaningful_lead(c))
        .trim();
    if stripped.is_empty() {
        None
    } else {
        let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        Some(cap_sidebar_title(&collapsed))
    }
}

fn cap_sidebar_title(title: &str) -> String {
    let mut chars = title.chars();
    let mut capped: String = chars.by_ref().take(MAX_SIDEBAR_TITLE_CHARS).collect();
    if chars.next().is_some() {
        capped.push('…');
    }
    capped
}

fn is_title_invisible_or_control(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200B}'
                | '\u{200C}'
                | '\u{200D}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// Whitelist of characters that can legitimately *start* a sidebar
/// thread title written by a human. Everything else (CLI status
/// glyphs, emoji, zero-width characters, format/control codepoints)
/// is treated as decoration and stripped by [`clean_sidebar_title`].
fn is_title_meaningful_lead(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            // Quotes -- ASCII + Unicode opening forms
            '"' | '\'' | '`'
            | '\u{201C}' | '\u{201D}'  // "" curly double
            | '\u{2018}' | '\u{2019}'  // '' curly single
            | '\u{00AB}' | '\u{00BB}'  // « » guillemets
            // Opening brackets / parens
            | '(' | '[' | '{'
            // Common title leads (hashtag, mention, code identifier)
            | '#' | '@' | '_'
            // Path / namespace separators
            | '/' | '\\' | '~' | '.'
            // Math / numeric leads
            | '-' | '+' | '=' | '$'
            | '\u{2013}' | '\u{2014}'  // - -
            | '\u{2212}'               // − minus sign
            // Currency
            | '\u{00A3}' | '\u{00A5}' | '\u{20AC}' // £ ¥ €
        )
}

/// Canonical string tag for an [`AgentKind`]. Stable on-disk format
/// for `ThreadSession::agent`.
pub fn agent_kind_to_str(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude_code",
        AgentKind::Codex => "codex",
    }
}

pub fn agent_kind_from_str(tag: &str) -> Option<AgentKind> {
    match tag {
        "claude_code" => Some(AgentKind::ClaudeCode),
        "codex" => Some(AgentKind::Codex),
        _ => None,
    }
}

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    #[test]
    fn thread_id_counter_is_monotonic() {
        let a = next_thread_id();
        let b = next_thread_id();
        let c = next_thread_id();
        assert!(a < b && b < c, "got {a} {b} {c}");
    }

    /// The invariant that replaced the `1 << 32` PTY-env namespace offset: a
    /// container id and a surface id can never be the same number, because
    /// they come from the same counter. An `ai.*` frame carrying a raw id
    /// therefore names exactly one thing, and a CLI pane's frame can never
    /// drive an Agents row's spinner.
    #[test]
    fn container_and_surface_ids_share_one_counter() {
        let mut seen = std::collections::HashSet::new();
        for id in [
            crate::workspace::next_workspace_id(),
            next_thread_id(),
            next_thread_id(),
            crate::workspace::next_workspace_id(),
            next_thread_id(),
        ] {
            assert!(seen.insert(id), "id {id} was issued twice");
        }
    }

    #[test]
    fn thread_status_maps_from_agent_state_without_text_labels() {
        assert_eq!(
            ThreadStatus::from_agent_state(crate::ai_types::AgentState::Thinking),
            ThreadStatus::Thinking
        );
        assert_eq!(
            ThreadStatus::from_agent_state(crate::ai_types::AgentState::Stalled),
            ThreadStatus::Thinking
        );
        assert_eq!(
            ThreadStatus::from_agent_state(crate::ai_types::AgentState::WaitingForInput),
            ThreadStatus::WaitingForInput
        );
        assert_eq!(
            ThreadStatus::from_agent_state(crate::ai_types::AgentState::Finished),
            ThreadStatus::Idle
        );
        assert_eq!(
            ThreadStatus::from_agent_state(crate::ai_types::AgentState::Errored),
            ThreadStatus::Failed
        );
    }

    // AC: monotonic ID atomicity across 1000 calls. We probe both
    // counters concurrently from many threads and assert (a) no
    // duplicates, (b) the full range was issued. The shared counter
    // in `super` is process-wide so other tests in the same binary
    // may have already advanced it; we work with the IDs we see, not
    // with a fixed range.
    #[test]
    fn container_id_atomic_no_duplicates_under_contention() {
        check_no_duplicates(crate::workspace::next_workspace_id, 1_000);
    }

    #[test]
    fn thread_id_atomic_no_duplicates_under_contention() {
        check_no_duplicates(next_thread_id, 1_000);
    }

    fn check_no_duplicates(make_id: fn() -> u64, total: usize) {
        let issued = Arc::new(std::sync::Mutex::new(Vec::with_capacity(total)));
        let threads_n = 8;
        let per_thread = total / threads_n;

        let handles: Vec<_> = (0..threads_n)
            .map(|_| {
                let issued = Arc::clone(&issued);
                thread::spawn(move || {
                    let mut local = Vec::with_capacity(per_thread);
                    for _ in 0..per_thread {
                        local.push(make_id());
                    }
                    issued.lock().unwrap().extend(local);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let mut ids = issued.lock().unwrap().clone();
        let observed = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), observed, "duplicate IDs issued under contention");
    }

    /// A container's parked agent surfaces survive the persisted shape.
    #[test]
    fn container_threads_roundtrip_through_session_shape() {
        let mut container = Workspace::detached("Splitlane", "/home/me/dev/splitlane");
        container.threads.push(Thread::new(
            "First thread",
            AgentKind::ClaudeCode,
            &container.cwd,
        ));
        container.threads.push(Thread::new(
            "Second thread",
            AgentKind::Codex,
            &container.cwd,
        ));

        let surfaces: Vec<ProjectSurface> = container
            .threads
            .iter()
            .map(|thread| thread_to_surface(thread, None))
            .collect();
        assert!(
            surfaces
                .iter()
                .all(|s| s.kind == SurfaceKind::Agent && s.placement == SurfacePlacement::Parked),
            "a thread persists as a parked agent surface"
        );
        assert_eq!(surfaces[0].agent.as_ref().unwrap().agent, "claude_code");
        assert_eq!(surfaces[1].agent.as_ref().unwrap().agent, "codex");

        let restored: Vec<Thread> = surfaces.iter().filter_map(thread_from_surface).collect();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].agent, AgentKind::ClaudeCode);
        assert_eq!(restored[1].agent, AgentKind::Codex);
        // Status always restores Idle regardless of pre-save value.
        assert_eq!(restored[0].status, ThreadStatus::Idle);
    }

    #[test]
    fn a_saved_diff_selection_still_resolves_by_stable_ids() {
        // Nothing writes `agents_target` any more, but a file written before
        // the diff became a pane's content still names it - and restore reads
        // that one arm to give the container back its `Changes` row.
        let mut first = Workspace::detached("First", "/tmp/first");
        first.id = 10;
        let mut second = Workspace::detached("Second", "/tmp/second");
        second.id = 20;
        let mut containers = vec![first, second];

        let saved = AgentsTargetSession::Diff { project_id: 20 };
        containers.swap(0, 1);
        let restored =
            agents_target_from_session(&saved, &containers).expect("target remaps after reorder");
        assert_eq!(restored, AgentsTarget::Diff { ws_idx: 0 });
    }

    /// A free chat written by an older build restores as what it now is: an
    /// agent surface of the container the fold put it in, found by the same
    /// stable id it was saved under.
    #[test]
    fn legacy_chat_target_restores_as_a_container_surface() {
        let mut home = Workspace::detached("Home", "/home/me");
        home.id = 10;
        let mut chat = Thread::new_terminal("Scratch", "/home/me", None);
        chat.id = 200;
        home.threads.push(chat);

        let saved = AgentsTargetSession::Chat { thread_id: 200 };
        let restored = agents_target_from_session(&saved, std::slice::from_ref(&home))
            .expect("a legacy chat target finds its surface");

        assert_eq!(
            restored,
            AgentsTarget::Thread {
                ws_idx: 0,
                thread_idx: 0,
            }
        );
    }

    #[test]
    fn missing_agents_target_id_restores_to_none() {
        let target = AgentsTargetSession::Thread {
            project_id: 1,
            thread_id: 404,
        };

        assert_eq!(agents_target_from_session(&target, &[]), None);
    }

    fn container_session(
        surfaces: Vec<ProjectSurface>,
    ) -> splitlane_config::schema::ProjectSession {
        splitlane_config::schema::ProjectSession {
            id: 1,
            title: "P".to_string(),
            cwd: "/tmp".to_string(),
            is_expanded: true,
            layout: None,
            surfaces,
            custom_buttons: Vec::new(),
            expanded_paths: Vec::new(),
            managed_worktrees: Vec::new(),
            preferred_agent: None,
            worktree_setup: None,
        }
    }

    fn agent_surface(id: u64, agent: &str) -> ProjectSurface {
        ProjectSurface {
            id,
            kind: SurfaceKind::Agent,
            placement: SurfacePlacement::Parked,
            title: format!("Thread {id}"),
            cwd: "/tmp".to_string(),
            created_at: 0,
            agent: Some(AgentSurface {
                agent: agent.to_string(),
                model: None,
                mode: None,
                store_id: None,
                thread_kind: None,
                terminal_agent: None,
                pinned: false,
                session_id: None,
                title_user_set: false,
            }),
        }
    }

    /// The container's diff surface shares the `surfaces` list with its agent
    /// surfaces, and the two must not bleed into each other: the diff row must
    /// never restore as a thread (a ghost surface sharing an id with the diff),
    /// and its id must be inside the max the restore path bumps the counter
    /// past (or the next surface is handed an id the file already uses).
    #[test]
    fn a_persisted_diff_surface_is_not_a_thread_and_holds_its_id() {
        let diff = diff_surface_record(4_242, "/tmp");
        assert_eq!(diff.kind, SurfaceKind::Diff);
        assert!(
            thread_from_surface(&diff).is_none(),
            "the diff is not a thread"
        );

        let session = container_session(vec![agent_surface(7, "claude_code"), diff]);
        let mut remaining = usize::MAX;
        assert_eq!(
            threads_from_session(&session, &mut remaining).len(),
            1,
            "only the agent surface restores as a thread"
        );
        assert_eq!(diff_surface_from_session(&session), Some(4_242));

        let state = splitlane_config::schema::SessionState {
            limits_reading: None,
            version: splitlane_config::schema::SESSION_SCHEMA_VERSION,
            active_workspace: 0,
            projects: vec![session],
            chats: Vec::new(),
            agents_target: None,
            diff_scope: None,
            rail_width: None,
            files_width: None,
        };
        assert_eq!(
            max_persisted_id(&state),
            4_242,
            "the diff surface's id counts towards the restore-time max"
        );
    }

    #[test]
    fn unknown_agent_tag_is_skipped_not_panicking() {
        // "opencode" is not in AgentKind yet.
        let session = container_session(vec![agent_surface(1, "opencode")]);
        let mut remaining = usize::MAX;
        // The unknown surface is silently dropped (forward-compat).
        assert!(threads_from_session(&session, &mut remaining).is_empty());
    }

    #[test]
    fn container_restore_respects_thread_budget() {
        let session =
            container_session((0..3).map(|id| agent_surface(id, "claude_code")).collect());
        let mut remaining = 2;

        let restored = threads_from_session(&session, &mut remaining);

        assert_eq!(restored.len(), 2);
        assert_eq!(remaining, 0);
    }

    /// The `pinned` flag survives a thread -> session -> thread
    /// round-trip, and a session shape without the flag restores `false`.
    #[test]
    fn pinned_flag_round_trips_through_session() {
        let mut thread = Thread::new_terminal(
            "Pinned",
            "/home/me",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
        assert!(!thread.pinned, "fresh threads start unpinned");
        thread.pinned = true;
        let session = thread_to_surface(&thread, None);
        assert!(
            session.agent.as_ref().unwrap().pinned,
            "pin flag persists into the surface shape"
        );
        let restored = thread_from_surface(&session).expect("terminal thread restores");
        assert!(restored.pinned, "pin flag restores from the session shape");
    }

    /// A surface showing in a slot keeps its session binding: placement says
    /// where it is, and says nothing about what it is. Getting this wrong is
    /// silent - the surface comes back looking right and cannot resume.
    #[test]
    fn slot_placement_does_not_disturb_the_session_binding() {
        let thread = Thread::new_terminal(
            "In a slot",
            "/home/me",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
        let bound = thread.session_id.clone().expect("Claude mints a uuid");

        let parked = thread_to_surface(&thread, None);
        let in_slot = thread_to_surface(&thread, Some(2));

        assert_eq!(parked.placement, SurfacePlacement::Parked);
        assert_eq!(in_slot.placement, SurfacePlacement::Slot { index: 2 });
        assert_eq!(
            in_slot.agent.as_ref().unwrap().session_id.as_deref(),
            Some(bound.as_str()),
            "moving a surface into a slot must not touch its session id"
        );
        assert_eq!(
            thread_from_surface(&in_slot)
                .expect("an agent surface restores")
                .session_id,
            Some(bound),
            "and it restores from the slot-placed record unchanged"
        );
    }

    #[test]
    fn claude_thread_mints_session_id_others_do_not() {
        let claude = Thread::new_terminal(
            "c",
            "/home/me",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
        let id = claude.session_id.as_deref().expect("Claude mints a uuid");
        assert!(
            crate::agent_sessions::is_valid_session_id(id),
            "minted id {id} must pass the PTY allow-list"
        );

        let codex = Thread::new_terminal(
            "x",
            "/home/me",
            Some(crate::agent_launcher::TerminalAgent::Codex),
        );
        assert!(
            codex.session_id.is_none(),
            "only forced-session-id agents mint an id"
        );
        let shell = Thread::new_terminal("s", "/home/me", None);
        assert!(shell.session_id.is_none());
    }

    #[test]
    fn session_id_and_rename_lock_round_trip() {
        let mut thread = Thread::new_terminal(
            "Backfilled",
            "/home/me",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
        thread.title_user_set = true;
        let session = thread_to_surface(&thread, None);
        let restored = thread_from_surface(&session).expect("terminal thread restores");
        assert_eq!(restored.session_id, thread.session_id, "forced id persists");
        assert!(restored.title_user_set, "manual-rename lock persists");
    }

    #[test]
    fn tampered_session_id_is_dropped_on_restore() {
        // A flag-shaped id smuggled into session.json collapses to None so
        // it can never reach `claude --session-id`.
        let mut thread = Thread::new_terminal(
            "T",
            "/home/me",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
        thread.session_id = Some("--dangerously-skip-permissions".to_string());
        let session = thread_to_surface(&thread, None);
        let restored = thread_from_surface(&session).expect("restores");
        assert!(
            restored.session_id.is_none(),
            "a flag-shaped session id must not survive restore"
        );
    }

    #[test]
    fn sidebar_titles_are_cleaned_and_bounded_before_persisting() {
        let long = "x".repeat(MAX_SIDEBAR_TITLE_CHARS + 8);
        let raw = format!("● hello\n\u{202E}world {long}");

        let cleaned = clean_sidebar_title(&raw).expect("title should survive cleanup");

        assert!(cleaned.starts_with("hello world "));
        assert!(!cleaned.contains('\n'));
        assert!(!cleaned.contains('\u{202E}'));
        assert_eq!(cleaned.chars().count(), MAX_SIDEBAR_TITLE_CHARS + 1);
        assert!(cleaned.ends_with('…'));

        let mut thread = Thread::new_terminal(raw, "/home/me", None);
        thread.title_user_set = true;
        let session = thread_to_surface(&thread, None);
        assert_eq!(session.title, cleaned);
    }

    /// Free chats draw ids from the same counter as a container's surfaces, so
    /// the restore-time max MUST fold them in - otherwise the next id collides
    /// with a chat that is sitting in the very file being read.
    #[test]
    fn max_persisted_id_covers_containers_surfaces_and_chats() {
        let session = splitlane_config::schema::SessionState {
            limits_reading: None,
            version: splitlane_config::schema::SESSION_SCHEMA_VERSION,
            active_workspace: 0,
            projects: vec![container_session(vec![agent_surface(
                1_000_000,
                "claude_code",
            )])],
            chats: vec![agent_surface(9_000_000, "claude_code")],
            agents_target: None,
            diff_scope: None,
            rail_width: None,
            files_width: None,
        };
        assert_eq!(max_persisted_id(&session), 9_000_000);

        crate::workspace::bump_next_id_to(max_persisted_id(&session) + 1);
        let next = next_thread_id();
        assert!(
            next > 9_000_000,
            "next_thread_id ({next}) must exceed the highest persisted id"
        );
    }

    #[test]
    fn bump_id_counters_advances_past_restored_max() {
        let raw = AtomicU64::new(5);
        crate::workspace::bump_counter(&raw, 10);
        assert_eq!(raw.load(Ordering::Relaxed), 10);
        // No regression: bumping to a smaller value is a no-op.
        crate::workspace::bump_counter(&raw, 7);
        assert_eq!(raw.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn agent_kind_tags_are_bijective() {
        for kind in AgentKind::all() {
            let tag = agent_kind_to_str(kind);
            assert_eq!(agent_kind_from_str(tag), Some(kind), "round-trip {tag}");
        }
        assert_eq!(agent_kind_from_str("nope"), None);
    }

    #[test]
    fn thread_from_history_adopts_the_picked_session() {
        // "Open a session from history": the thread must carry the picked id
        // verbatim, because that is what makes its first PTY mount resolve to
        // `SessionBinding::Resume` and bring the conversation back.
        const PICKED: &str = "6b030d25-d996-45cb-a270-f1d9c2a0610f";
        let thread = Thread::new_terminal_for_session(
            "Fable coordination",
            "/tmp/proj",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
            PICKED,
        );
        assert_eq!(thread.session_id.as_deref(), Some(PICKED));
        assert_eq!(thread.kind, ThreadKind::Terminal);
    }

    #[test]
    fn fresh_thread_mints_an_id_distinct_from_any_history() {
        let a = Thread::new_terminal(
            "One",
            "/tmp/proj",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
        let b = Thread::new_terminal(
            "Two",
            "/tmp/proj",
            Some(crate::agent_launcher::TerminalAgent::ClaudeCode),
        );
        assert!(a.session_id.is_some());
        assert_ne!(
            a.session_id, b.session_id,
            "two fresh threads must never share a session file"
        );
    }

    #[test]
    fn non_claude_thread_never_adopts_a_session_id() {
        // Codex has no `--session-id`, so handing it one would put an id on
        // the thread that no launch could ever honour.
        let thread = Thread::new_terminal_for_session(
            "Codex",
            "/tmp/proj",
            Some(crate::agent_launcher::TerminalAgent::Codex),
            "6b030d25-d996-45cb-a270-f1d9c2a0610f",
        );
        assert_eq!(thread.session_id, None);

        let bare = Thread::new_terminal_for_session(
            "Shell",
            "/tmp/proj",
            None,
            "6b030d25-d996-45cb-a270-f1d9c2a0610f",
        );
        assert_eq!(bare.session_id, None);
    }
}
