//! One question about a running agent: has it spawned a worker?
//!
//! This is the second input to [`crate::agent_state::classify`], and the only
//! one that does not come from a file an agent wrote. It is deliberately the
//! narrowest question that answers what the transcript cannot: a `Bash` call
//! sitting open means "the command is running" if there is a child process
//! under the agent, and "the command has not started, so somebody is being
//! asked" if there is not.
//!
//! # Why children and not CPU
//!
//! Measured on a live agent over 80 samples: with no tool running at all it
//! ranged from 0.4% to 36% CPU, because thinking is work too - so CPU cannot
//! separate a busy agent from a waiting one. Children separate where CPU does
//! not: while a command runs, the shell it runs in appears under the agent.
//!
//! # What that measurement missed, and what it costs
//!
//! It used to continue "and while none ran it had **none**. Agent CLIs keep no
//! long-lived helper processes of their own, so there is nothing to filter out
//! and no allowlist to maintain." Measured again on 26 August against Claude
//! Code 2.1.246, both sentences are false:
//!
//! - a **stdio MCP server** is a child of the agent for the entire session, so
//!   [`ProcessSnapshot::worker_under`] answers [`Worker::Present`] forever for
//!   anyone who has one configured - and `splitlane mcp install` configures one,
//!   which makes this our own risk rather than a hypothetical;
//! - `caffeinate` is spawned on prompt submit and held for about 37 s, on a
//!   profile with no MCP servers at all.
//!
//! Neither puts a false claim in the rail - both push every answer towards
//! "working", which is the safe direction - but the first removes the "waiting
//! for a person" answer entirely for a whole class of users, silently.
//!
//! The decision that followed is that the boolean question is the wrong one:
//! "has any child" cannot tell a structural child from a worker, and no amount
//! of resolving *which* pid is the agent helps, because all of this sits
//! **below** a correctly resolved agent. What replaced it is [`worker_against`]:
//! a baseline of the subtree snapshotted when the transcript says the turn is
//! over, with a worker being any descendant that is not in it. That landed in
//! `65eb4fe`, and [`ProcessSnapshot::worker_under`] is scoped to the tests.
//!
//! It is verified live: with our own stdio bridge running as a child of
//! the agent, a `Write` ask reached [`Worker::Absent`] and the rail said
//! "waiting for you" - the answer that was unreachable for such a user before.
//!
//! # Whose pid to ask about
//!
//! Not the one on the thread. `Thread::agent_pid` comes from `SPLITLANE_AI_PID`,
//! which the shim sets to **its own** pid on purpose
//! (`crates/splitlane-shim/src/exec.rs`: the child's pid is not known until
//! after the spawn, and the shim outlives the child for the stale sweep). The
//! shim then spawns the agent and waits, so it always has exactly one child and
//! [`ProcessSnapshot::worker_under`] would answer `Present` forever - the
//! `Spawns` arm of the rule would never fire, silently.
//!
//! [`agent_under_pty`] is the answer: walk down from the pane's PTY child and
//! step over our own shim. Two things about the shape are measured on this
//! machine rather than assumed, and both differ from what the plan for this
//! work said:
//!
//! - **The PTY child is the user's shell, not the shim.** The agent's launch
//!   command is typed into that shell (`agents_view_actions.rs`), so the chain
//!   is `shell → shim → agent`, and while a command runs it is
//!   `shell → shim → agent → zsh → …`.
//! - **No process is named `splitlane-shim`.** The shim binary is staged under
//!   each agent's own name (`ai_hooks::extract::extract_plan` copies it to
//!   `claude`, `codex`, …), so the shim and the agent share a basename and the
//!   name separates nothing. What does separate them is the **executable
//!   path**: the shim's is inside the directory this app staged it into, and no
//!   vendor binary can be. Verified against the real shim: its `txt` fd is the
//!   staged path, while `argv[0]` is the bare agent name.
//!
//! # Why this is its own module
//!
//! `workspace/ports.rs` already walks process trees, and its per-OS primitives
//! are the model these follow. It is not the home for this: that module is the
//! TCP port scan, and hanging an unrelated question off its name is how a file
//! ends up meaning two things. If a third caller ever needs a process tree, the
//! three implementations there and here should merge - into this name, not that
//! one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::agent_state::Worker;

/// How far down from a PTY child the shim is looked for, in processes.
///
/// It is normally the first child, so this is a bound and not a budget: what it
/// buys is that a pane running a build with hundreds of descendants cannot turn
/// one thread's question into a walk of the whole tree. Overrunning it yields
/// no shim and therefore [`Worker::Unknown`], which claims nothing.
const MAX_WALK: usize = 128;

/// How many processes a subtree may hold before its shape stops being readable.
///
/// Higher than [`MAX_WALK`] because this walk is the whole subtree rather than
/// a search for one process in it, and a pane running a build legitimately has
/// hundreds. Overrunning yields `None` - which is [`Worker::Unknown`] - rather
/// than a partial set, because a partial set reads as "fewer workers".
const MAX_DESCENDANTS: usize = 4096;

/// One reading of the machine's process table, taken once per state pass.
///
/// Per-pass rather than per-question because every platform answers "who are
/// this pid's children" by some form of full enumeration - a `/proc` scan, a
/// `sysctl` of the whole process table, a Toolhelp snapshot - so asking it once
/// per agent surface would repeat that walk for every pane on screen. One
/// capture answers every surface's question, and it is also one consistent
/// picture: two pids read from the same table cannot disagree about who is
/// whose parent.
/// What a platform's enumeration hands back: who is whose child, each pid's
/// start token where the walk already carries one, and whether the walk saw
/// every process on the machine.
struct ProcessTable {
    children: HashMap<u32, Vec<u32>>,
    starts: HashMap<u32, u64>,
    complete: bool,
}

pub(crate) struct ProcessSnapshot {
    children: HashMap<u32, Vec<u32>>,
    /// Per-pid start token, where the platform hands one back for free during
    /// the walk. Empty on platforms whose enumeration does not carry it, in
    /// which case [`ProcessSnapshot::identity_of`] asks per pid instead.
    starts: HashMap<u32, u64>,
    /// Whether this reading saw **every** process the machine had **at the
    /// instant it was taken**.
    ///
    /// The instant is part of the claim and not a caveat on it: every platform
    /// answers with a snapshot, so a process that starts after the read is not
    /// a gap in the reading - it did not exist when the reading was made. What
    /// this flag is about is a process that *was* there and did not appear.
    ///
    /// This is the one property [`descendants_of`]'s per-child guard cannot
    /// establish for itself. That guard refuses a child it cannot *name*; a
    /// child the platform never *listed* is missing from `children` before the
    /// guard is reached, so an unlisted worker would leave the difference empty
    /// and read as no worker at all - a person reported as waiting while a
    /// command runs, which is the one direction this detector may not err in.
    ///
    /// So the enumeration says whether it was whole, and [`worker_against`]
    /// refuses [`Worker::Absent`] when it was not. Losing the signal is a
    /// missed point; keeping it would be a false one.
    complete: bool,
}

/// A process's identity: its pid **and** the moment it started.
///
/// The pid alone is not an identity. It is unique only among processes alive at
/// one instant, and the OS recycles it - macOS wraps at ~99 999, which a machine
/// that compiles all day walks through in hours. A baseline holding a bare pid
/// therefore starts naming whatever process inherits that number, and if that
/// process is a worker the baseline hides it, which reports a person waiting
/// while a command runs.
///
/// The token is deliberately **opaque** and never converted to a wall clock.
/// Comparing a process's start against a timestamp of ours would need the two
/// to share a clock frame, and they do not: Linux counts ticks since boot, so
/// the conversion goes through `btime` and an NTP step or a suspended laptop
/// shifts every answer at once - towards a false "waiting", in the middle of a
/// long build. Identity only ever compares equal against equal, inside one
/// snapshot, so no clock frame is involved and none of that can happen.
pub(crate) type ProcId = (u32, u64);

impl ProcessSnapshot {
    /// Read the process table, or `None` where the platform cannot be asked.
    ///
    /// Blocking. Call it off the render thread.
    pub(crate) fn capture() -> Option<Self> {
        capture_impl().map(|table| Self {
            children: table.children,
            starts: table.starts,
            complete: table.complete,
        })
    }

    /// Whether this reading saw every process on the machine.
    ///
    /// False only where a platform can hide one from us and has: Linux under
    /// `hidepid`, or a `/proc` entry this user may not read. macOS reads the
    /// table through `sysctl(KERN_PROC_ALL)`, which answers for every process
    /// whatever its owner, and Windows' Toolhelp walk enumerates the whole
    /// machine too - on both, a process we cannot *identify* is still one we
    /// can see, and [`ProcessSnapshot::identity_of`] refuses it by name.
    fn complete(&self) -> bool {
        self.complete
    }

    /// This pid's identity, or `None` when it cannot be established.
    ///
    /// `None` is never "no worker" - every caller must carry it to
    /// [`Worker::Unknown`]. A process whose start we cannot read is a process
    /// we cannot tell apart from a recycled pid.
    pub(crate) fn identity_of(&self, pid: u32) -> Option<ProcId> {
        if pid == 0 {
            return None;
        }
        match self.starts.get(&pid) {
            Some(&start) => Some((pid, start)),
            // Windows' Toolhelp walk carries no start time, so it is asked per
            // pid here instead. The sets this is used on are a handful of
            // processes, not the whole table.
            None => start_token_impl(pid).map(|start| (pid, start)),
        }
    }

    /// Every descendant of `root`, by identity, or `None` if the walk could not
    /// be completed or trusted.
    ///
    /// `None` on overrun rather than a truncated set, because a short set is
    /// indistinguishable from a small one and would read as "no worker".
    pub(crate) fn descendants_of(&self, root: u32) -> Option<Vec<ProcId>> {
        if root == 0 {
            return None;
        }
        let mut out = Vec::new();
        let mut queue = std::collections::VecDeque::from([root]);
        let mut seen = std::collections::HashSet::from([root]);
        let mut visited = 0usize;
        while let Some(pid) = queue.pop_front() {
            visited += 1;
            if visited > MAX_DESCENDANTS {
                return None;
            }
            for &child in self.children_of(pid) {
                if !seen.insert(child) {
                    continue;
                }
                // A descendant that is listed but cannot be named makes the
                // whole set untrustworthy, because dropping it silently is how
                // an invisible worker becomes "no worker".
                //
                // This guard is about a child that was **listed**. The child
                // that was never listed is a different question and cannot be
                // asked here - it is missing from `children_of`, so nothing in
                // this walk can see it. That one is answered once, by the
                // enumeration itself: [`ProcessSnapshot::complete`] says
                // whether the reading was whole, and [`worker_against`] refuses
                // `Absent` when it was not.
                out.push(self.identity_of(child)?);
                queue.push_back(child);
            }
        }
        Some(out)
    }

    /// The children of `pid`, in whatever order the platform listed them.
    pub(crate) fn children_of(&self, pid: u32) -> &[u32] {
        self.children.get(&pid).map_or(&[], Vec::as_slice)
    }

    /// Whether `pid` currently has any child process.
    ///
    /// **Not a valid input to the detector, and no longer reachable from it.**
    /// This was the second signal the rule ran on, and it is wrong for every
    /// user who has a stdio MCP server configured: the server is a child of the
    /// agent for the whole session, so this answers [`Worker::Present`] for ever
    /// and no arm of [`crate::agent_state::classify`] can report a person
    /// waiting. [`worker_against`] is the question that replaced it.
    ///
    /// Kept for the tests below, which are about the **shape of the tree** -
    /// "does this pid have a child" is still exactly the right question to ask
    /// of a fixture. Scoped to tests so that it cannot drift back into the pass.
    /// The same reading with its completeness flag cleared.
    ///
    /// The condition it stands in for cannot be produced on demand - it is
    /// another user's process going unlisted, which needs a `hidepid` mount or
    /// a kernel that refuses us - so the flag is set by hand and the behaviour
    /// it gates is what gets tested.
    #[cfg(test)]
    fn into_incomplete(mut self) -> Self {
        self.complete = false;
        self
    }

    #[cfg(test)]
    fn worker_under(&self, pid: u32) -> Worker {
        // A pid of zero is not a process anyone can ask about, and on some
        // platforms it means "every process in the group" - a question with a
        // very different answer than the one being asked.
        if pid == 0 {
            return Worker::Unknown;
        }
        if self.children_of(pid).is_empty() {
            Worker::Absent
        } else {
            Worker::Present
        }
    }
}

/// The pid of the agent itself under a pane's PTY child, stepping over our own
/// shim.
///
/// `shim_dir` is the directory this build staged its shim binaries into
/// (`ai_hooks::extract::ensure_binaries_extracted`). A process whose executable
/// lives there is ours by construction, whatever it calls itself.
///
/// `None` means "could not tell", and every caller must read it as
/// [`Worker::Unknown`] rather than as an answer. It is returned whenever the
/// shape is not the one measured - no shim under this PTY (the user started the
/// agent some other way, or started none), more than one (two agents in one
/// shell), or a shim that does not have exactly one child (it is still
/// spawning, or its agent has already exited). Guessing in any of those cases
/// would put a claim about a person in the rail on the strength of a shape
/// nobody has measured.
///
/// # The shim's single child is not always the agent
///
/// Measured on this machine, and it is the trap waiting for the next reader.
/// Claude Code is installed as a native binary, so the chain is exactly
/// `shell → shim → claude` and the single child **is** the agent - which is why
/// the one agent this build reads is read correctly. Codex is installed through
/// npm, and its chain is `shell → shim → node → codex`: the wrapper sits
/// between, and it always has the real binary under it. Asked about a Codex
/// PTY, this function returns the **wrapper**, and [`Worker::Present`] then
/// holds for ever - the same silent failure the shim itself caused, one level
/// further down.
///
/// It costs nothing today, because a surface is only ever asked about when
/// [`crate::agent_launcher::TerminalAgent::reports_state`] is true and that is
/// Claude Code alone. It will cost the Codex reader everything, so: **measure
/// the chain before adding an agent to `reports_state`**. Descending blindly
/// past the wrapper is not the fix - that is the rejected "chain of only
/// children", which walks past a real agent the moment it runs a command.
///
/// # Why not the alternatives
///
/// Descending the chain of only-children walks past the agent the moment a
/// command runs, because the chain grows a `zsh` below it.
///
/// **Counting** descendants against a self-calibrating baseline is worse: the
/// baseline starts too high until the first pause, and the error runs towards a
/// false "waiting", which is the one direction this detector may not err in.
///
/// That rejection is about a *count*, and the later measurement draws the line the
/// original wording blurred: a baseline held as a **set of process identities**
/// does not share the flaw, because a stale entry names a process that is gone
/// rather than raising a threshold every future worker must clear.
///
/// **"Set of pids" is not enough, and the first draft of this note said it
/// was.** A pid is unique only among *live* processes: the OS recycles it, and
/// on macOS the space is ~99 999, which a machine that compiles all day walks
/// through in hours. A recycled pid that is still sitting in the baseline
/// names a **new** process - and if that process is the worker, the worker is
/// invisible, the difference is empty, and the `Spawns` arm reports a person
/// waiting while a command runs. That is the one direction this detector may
/// not err in, and it arrives through the very mechanism meant to fix it.
///
/// So the baseline is keyed by **(pid, start time)**, which costs nothing to
/// carry: `pbi_start_tvsec` is already in the `BSDInfo` this module reads for
/// `pbi_ppid`, and Linux's `starttime` is field 22 of the same `/proc/<pid>/stat`
/// line. Only Windows pays extra, needing `GetProcessTimes` beside the Toolhelp
/// walk. A descendant whose start time cannot be read is not evidence and must
/// read as [`Worker::Unknown`], never [`Worker::Absent`].
///
/// # Whatever replaces this function must keep what it *refuses* to answer
///
/// The trap found while designing that replacement, and worth more than the
/// design: this function's value is not only the pid it resolves, it is the
/// `None` it returns when the shape is wrong. A shim with anything other than
/// exactly one child yields `None` and therefore [`Worker::Unknown`] - which is
/// what stops a **dead** agent from reading as [`Worker::Absent`]. That matters
/// because a dead agent leaves its transcript frozen with a call still open,
/// and `Absent` under an open `Spawns` call is a person reported as waiting,
/// held for `TRUST_HORIZON` - half an hour of a rail pointing at a session that
/// does not exist.
///
/// Rooting a baseline at the PTY child drops that guard silently: if the shim
/// dies, the agent reparents to init and leaves the subtree entirely, so the
/// difference comes back empty and reads as "no worker" rather than "no
/// subtree". Any replacement therefore owes a liveness guard - the baseline
/// must still hold at least one live long-lived entry, and a baseline whose
/// entries all died at once means `Unknown`, never `Absent`. Recorded because
/// the omission came from looking at what resolving the agent *provides* rather
/// than at what it *declines to say*.
pub(crate) fn agent_under_pty(
    snapshot: &ProcessSnapshot,
    pty_child: u32,
    shim_dir: &Path,
) -> Option<u32> {
    agent_under_pty_by(snapshot, pty_child, &|pid| is_ours(pid, shim_dir))
}

/// [`agent_under_pty`] with the "is this ours" question handed in.
///
/// Split out so the walk can be tested against a real process tree: on macOS a
/// copy of a system binary is SIGKILLed on exec, so a test cannot stage a
/// stand-in shim by copying one - and a shell script will not do either,
/// because the executable path of a process running a script is the
/// interpreter's. The predicate itself is exercised by
/// `the_agent_is_found_under_the_real_shim`.
fn agent_under_pty_by(
    snapshot: &ProcessSnapshot,
    pty_child: u32,
    is_shim: &dyn Fn(u32) -> bool,
) -> Option<u32> {
    if pty_child == 0 {
        return None;
    }
    let mut queue = std::collections::VecDeque::from([pty_child]);
    let mut seen = std::collections::HashSet::from([pty_child]);
    let mut visited = 0usize;
    let mut shim: Option<u32> = None;
    while let Some(pid) = queue.pop_front() {
        visited += 1;
        if visited > MAX_WALK {
            return None;
        }
        if is_shim(pid) {
            if shim.is_some() {
                // Two shims under one PTY: the shell has started a second
                // agent, and nothing here says which surface asked.
                return None;
            }
            shim = Some(pid);
            // Everything below a shim is its agent's own tree, which cannot
            // hold another shim of interest - and on a busy agent it is where
            // all the processes are.
            continue;
        }
        for &child in snapshot.children_of(pid) {
            if seen.insert(child) {
                queue.push_back(child);
            }
        }
    }
    let shim = shim?;
    match snapshot.children_of(shim) {
        [agent] => Some(*agent),
        _ => None,
    }
}

/// What a surface's subtree looked like when its agent last finished a turn.
///
/// Everything under a pane's PTY child that is **not** a worker is stable for
/// the life of the session: the shim, an npm wrapper, the agent itself, and any
/// stdio MCP server the agent keeps. None of them can be recognised by name
/// without a list this project has already rejected as its own fragility - so
/// they are recognised by *having been there* when nothing was running.
///
/// Taken at the end of a turn, refreshed at the end of every turn, and rooted
/// at the **PTY child** rather than at the resolved agent. Rooting it there is
/// what makes it indifferent to the shape of the launch: shim, wrapper and
/// agent all fall inside it, so nothing has to be identified.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkerBaseline {
    /// The PTY child's own identity, so a remounted surface - or a recycled pid
    /// landing on this number - invalidates the baseline instead of inheriting
    /// somebody else's subtree.
    root: ProcId,
    /// The identities that were present with nothing running.
    structural: std::collections::HashSet<ProcId>,
}

impl WorkerBaseline {
    /// Snapshot the subtree as structural, or `None` if it cannot be read
    /// completely. A partial baseline is worse than none: what it misses
    /// becomes a permanent "worker".
    pub(crate) fn take(snapshot: &ProcessSnapshot, pty_child: u32) -> Option<Self> {
        let root = snapshot.identity_of(pty_child)?;
        let structural = snapshot.descendants_of(pty_child)?.into_iter().collect();
        Some(Self { root, structural })
    }
}

/// Whether a worker is running under a pane, judged against what the subtree
/// looked like when the turn ended.
///
/// # Every uncertainty answers [`Worker::Unknown`]
///
/// There is one shape that yields [`Worker::Absent`] - a complete reading of a
/// live subtree whose every member was there when nothing was running - and
/// every other outcome claims nothing. That asymmetry is the invariant: only
/// `Absent` can put "a person is being kept waiting" in the rail, so only
/// positive, complete evidence is allowed to produce it.
///
/// "Complete" is meant of the machine's whole table and not only of this
/// subtree, because the two cannot be told apart from in here: a process the
/// platform never listed is absent from `children_of`, so a subtree missing one
/// looks exactly like a subtree that has none. Whether the enumeration was
/// whole is therefore carried from where it happened
/// ([`ProcessSnapshot::complete`]) rather than re-derived here.
///
/// # A baseline taken from an incomplete table is not the same risk
///
/// [`WorkerBaseline::take`] is deliberately **not** gated on completeness. What
/// an incomplete baseline misses is a structural process, which then reads as
/// new - [`Worker::Present`], the safe direction - and is corrected at the next
/// finished turn. The gate belongs on the answer that can lie, and there is one.
pub(crate) fn worker_against(
    snapshot: &ProcessSnapshot,
    baseline: Option<&WorkerBaseline>,
    pty_child: u32,
) -> Worker {
    // A process this reading never saw could be the worker, and no comparison
    // made below can notice its absence. Asked first because it disqualifies
    // the whole table rather than anything about this pane.
    if !snapshot.complete() {
        return Worker::Unknown;
    }
    // No turn has ended yet, so nothing is known about this subtree's resting
    // shape and every process in it looks equally suspicious.
    let Some(baseline) = baseline else {
        return Worker::Unknown;
    };
    // A different process is sitting on this pid now, or the pane was
    // remounted: the baseline describes somebody else's subtree.
    if snapshot.identity_of(pty_child) != Some(baseline.root) {
        return Worker::Unknown;
    }
    let Some(now) = snapshot.descendants_of(pty_child) else {
        return Worker::Unknown;
    };
    if now.iter().any(|id| !baseline.structural.contains(id)) {
        Worker::Present
    } else {
        Worker::Absent
    }
}

/// Whether `pid` is running an executable this build staged.
fn is_ours(pid: u32, shim_dir: &Path) -> bool {
    exe_path_of(pid).is_some_and(|exe| exe.starts_with(shim_dir))
}

/// The path of the executable `pid` is running.
///
/// Deliberately the executable and not `argv[0]`: the shim is invoked through
/// `$PATH` under the agent's own name, so its `argv[0]` is `claude` and tells
/// us nothing, while its executable path is the file we wrote.
fn exe_path_of(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    exe_path_impl(pid)
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

/// `/proc` is the whole answer where the kernel lets it be: every entry's
/// `PPid` names its parent.
///
/// # Where it is not the whole answer, and how this says so
///
/// Linux has no `sysctl(KERN_PROC_ALL)` to fall back to, so the macOS fix has
/// no counterpart here and the honest move is to report the gap rather than
/// close it. `hidepid` is what opens it, in two shapes:
///
/// - `hidepid=1`, where another user's `/proc/<pid>` is listed but not
///   enterable, so the walk sees the entry and the read is denied;
/// - `hidepid=2` (`invisible`), where the entry is not listed at all and there
///   is nothing here to notice.
///
/// The first is caught directly, by the errno of the read that failed. The
/// second cannot be - so it is caught by asking whether **`/proc/1`** can be
/// read: pid 1 is root's, so under either `hidepid` an unprivileged user cannot
/// reach it, and under neither is it hidden. Running as root, or as pid 1 of a
/// namespace that holds only our own processes, reads fine and is complete,
/// which is the right answer in both cases.
///
/// An incomplete table costs [`Worker::Absent`] and therefore "waiting for
/// you", which is a real loss on such a machine. It is the loss the detector's
/// rule asks for - never claim a person is needed when it only lacks evidence:
/// a missed point rather than a false one.
#[cfg(target_os = "linux")]
fn capture_impl() -> Option<ProcessTable> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut starts: HashMap<u32, u64> = HashMap::new();
    let mut incomplete = false;
    for entry in entries {
        // The directory stream can fail part-way through, and this used to be
        // an `entries.flatten()`, which drops the `Err` and walks on. What it
        // drops is an entry nobody ever saw - a process missing from a table
        // that would still call itself whole, which is this function's one
        // forbidden outcome. It says so instead.
        let Ok(entry) = entry else {
            incomplete = true;
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A per-pid read failing is routine when the process exited between the
        // listing and the read, and is something else entirely when it was
        // refused: `/proc/<pid>/stat` is world-readable by default, so a denial
        // is a restriction and the table cannot be called whole. Only a failure
        // to list `/proc` at all means "cannot answer".
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => match parse_stat(&stat) {
                ProcStat::Live { ppid, start } => {
                    starts.insert(pid, start);
                    if ppid != pid {
                        children.entry(ppid).or_default().push(pid);
                    }
                }
                // Left out on purpose, and not a gap: see `parse_stat`.
                ProcStat::Zombie => {}
                // Listed, readable, and not understood - which removes a
                // process from the table exactly the way `hidepid` does, and
                // must be admitted the same way. Kept distinct from a zombie
                // for the reason that makes this worth writing down: a zombie
                // is also a line this function declines to return facts for,
                // and treating the two alike would mark the table incomplete on
                // any machine that had one - taking "waiting for you" away for
                // a state that is ordinary and understood.
                ProcStat::Unreadable => incomplete = true,
            },
            // `NotFound` is the one routine failure: the process exited
            // between the listing and the read, so there was nothing to miss.
            // Everything else - a denial under `hidepid=1`, an I/O error, an
            // LSM refusing this one pid - is a listed process this reading
            // could not account for, and the table may not call itself whole.
            // It was only `PermissionDenied`, which named the cause this work
            // came from rather than the property being claimed.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => incomplete = true,
        }
    }
    Some(ProcessTable {
        children,
        starts,
        complete: !incomplete && proc_lists_every_process(),
    })
}

/// Whether `/proc` is listing processes this user does not own.
///
/// One read of `/proc/1/stat`, which is root's on any ordinary system and this
/// process's own inside a pid namespace - readable in both, and unreachable
/// under `hidepid` for anybody else. See [`capture_impl`] for why the listing
/// itself cannot answer this.
#[cfg(target_os = "linux")]
fn proc_lists_every_process() -> bool {
    std::fs::read_to_string("/proc/1/stat").is_ok()
}

/// ppid is field 4 of `/proc/<pid>/stat`, taken after the LAST `)`: field 2 is
/// the comm, it is parenthesized, and it may itself contain spaces and parens.
/// This is the kernel-documented safe parse, and the same one `ports.rs` uses.
/// Both facts come from one line, because they are on one line: ppid is field 4
/// and `starttime` is field 22, counted after the LAST `)`. Field 2 is the
/// comm, it is parenthesized, and it may itself contain spaces and parens -
/// this is the kernel-documented safe parse, and the same one `ports.rs` uses.
///
/// `starttime` is left in the clock ticks since boot that the kernel reports.
/// It is an identity token, not a time: converting it to a wall clock would
/// need `btime`, and then an NTP step or a resumed laptop would move every
/// process's apparent start at once. See [`ProcId`].
#[cfg(target_os = "linux")]
fn stat_of(pid: u32) -> Option<(u32, u64)> {
    match parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?) {
        ProcStat::Live { ppid, start } => Some((ppid, start)),
        ProcStat::Zombie | ProcStat::Unreadable => None,
    }
}

/// What one `/proc/<pid>/stat` line says, in the three shapes the caller has to
/// tell apart.
///
/// Split out from the read so that [`capture_impl`] can tell a denied read from
/// a missing one - which is the whole of how `hidepid=1` is detected - and
/// three-valued rather than an `Option` so it can also tell a **zombie**, which
/// is left out deliberately, from a line this build did not understand, which
/// is a table that must admit it is incomplete.
#[cfg(target_os = "linux")]
enum ProcStat {
    Live { ppid: u32, start: u64 },
    Zombie,
    Unreadable,
}

#[cfg(target_os = "linux")]
fn parse_stat(stat: &str) -> ProcStat {
    let Some(paren) = stat.rfind(')') else {
        return ProcStat::Unreadable;
    };
    let mut fields = stat[paren + 1..].split_whitespace();
    // Field 3 is the state, and `Z` is a process that has exited and not been
    // reaped. `/proc` lists one and answers for it, so unlike macOS this walk
    // has always carried them - and a zombie PTY child then has a readable
    // identity, matches its baseline root, and presents an empty subtree, which
    // is `Absent` and therefore "waiting for you" in the rail for a pane whose
    // shell is dead. It holds nothing and runs nothing, so it is left out here
    // and has no identity at all. See the macOS `live_entry` for the same rule
    // and the measurement behind it.
    let Some(state) = fields.next() else {
        return ProcStat::Unreadable;
    };
    if state.starts_with('Z') {
        return ProcStat::Zombie;
    }
    // ppid is the next field, and the walk to field 22 is counted from the same
    // iterator so the two cannot disagree.
    let parsed = fields
        .next()
        .and_then(|ppid| ppid.parse::<u32>().ok())
        .zip(fields.nth(17).and_then(|start| start.parse::<u64>().ok()));
    match parsed {
        Some((ppid, start)) => ProcStat::Live { ppid, start },
        None => ProcStat::Unreadable,
    }
}

/// Every Linux pid's token is already in the table from [`capture_impl`]; this
/// is the fallback for a pid asked about after it appeared.
#[cfg(target_os = "linux")]
fn start_token_impl(pid: u32) -> Option<u64> {
    stat_of(pid).map(|(_, start)| start)
}

#[cfg(target_os = "linux")]
fn exe_path_impl(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

/// Read the whole table in one `sysctl`, which is the only macOS enumeration
/// that answers for a process this user does not own.
///
/// # Why not the `pidinfo` loop this replaces
///
/// It was `pids_by_type(ProcFilter::All)` plus a `proc_pidinfo` per pid, and
/// that per-pid call is privilege-checked: it answers `EPERM` for another
/// user's process, and the loop dropped every pid it refused. Measured on this
/// machine on 7 September, 877 pids listed and **294 refused** - so 294
/// processes were present on the machine and absent from the map, every one of
/// them another user's.
///
/// That is not a gap in coverage of things we do not care about. Our own
/// descendants are same-user and read fine, but a `sudo` under an agent's
/// command is **root-owned and inside our subtree**, so it never became
/// anybody's child here, `descendants_of` never saw it, the difference against
/// the baseline came back empty, and the `Spawns` arm reported a person waiting
/// while the command ran. A false "waiting for you" is the one thing this
/// detector may not produce: it must never claim a person is needed when it
/// only lacks evidence.
///
/// # What was measured before trusting this
///
/// `KERN_PROC_ALL` returns 878 entries in one call, carrying **both** facts
/// this module needs - `e_ppid` and `p_starttime` - for every process whatever
/// its owner. Against the 567 pids `proc_pidinfo` could still read, the two
/// agree on pid, on ppid, and on the start token this module builds, 567 out of
/// 567 with no disagreement. So this is the same reading, extended to the
/// processes the old one could not see, and it costs two syscalls instead of
/// 878.
///
/// # Why the fields are read by offset
///
/// `libc` declares `KERN_PROC_ALL` but not `struct kinfo_proc`, and the three
/// fields wanted sit inside 648 bytes of nested BSD structures. Transcribing
/// all of it to reach three integers would be a large declaration whose
/// correctness nothing checks. Instead the offsets are named in [`kinfo`] and
/// **proved at runtime** by [`kinfo_stride`], against a process whose pid and
/// parent are already known - and a probe that does not come back right yields
/// `None`, which is [`Worker::Unknown`] everywhere, never a claim.
#[cfg(target_os = "macos")]
fn capture_impl() -> Option<ProcessTable> {
    let stride = kinfo_stride()?;
    let table = kern_proc_all()?;
    // A table that is not a whole number of entries is one this build does not
    // understand, and `chunks_exact` would answer by dropping the tail - which
    // is a missing process, which is the whole defect this function exists to
    // close. Say "cannot answer" instead.
    if table.len() % stride != 0 {
        return None;
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut starts: HashMap<u32, u64> = HashMap::new();
    for entry in table.chunks_exact(stride) {
        let Some((pid, ppid, start)) = live_entry(entry) else {
            continue;
        };
        starts.insert(pid, start);
        if ppid != pid {
            children.entry(ppid).or_default().push(pid);
        }
    }
    // Every process, whoever owns it: there is no per-pid permission check on
    // this path to drop one.
    Some(ProcessTable {
        children,
        starts,
        complete: true,
    })
}

/// Where the three facts this module needs sit inside `struct kinfo_proc`.
///
/// Taken from `<sys/sysctl.h>` and confirmed by measurement on this machine:
/// `kp_proc` is a `struct extern_proc` whose head is a union of two list
/// pointers with the start `timeval`, so `p_starttime` is at 0 and its
/// `tv_usec` at 8; `p_pid` is at 40; and `kp_eproc.e_ppid` is at 560 of the
/// 648-byte whole.
///
/// These are an ABI the kernel publishes to userland rather than a private
/// layout, but nothing here assumes they cannot move: [`kinfo_stride`] checks
/// two of the three against facts the process already knows about itself before
/// any of them is believed.
#[cfg(target_os = "macos")]
mod kinfo {
    /// `kp_proc.p_starttime.tv_sec`, a 64-bit `time_t`.
    pub(super) const START_SEC: usize = 0;
    /// `kp_proc.p_starttime.tv_usec`, a 32-bit `suseconds_t`.
    pub(super) const START_USEC: usize = 8;
    /// `kp_proc.p_stat`, one byte. Measured at 36 and **not** at the 37 that
    /// counting the fields by hand suggests, which is the whole reason these
    /// are measured rather than derived.
    pub(super) const STAT: usize = 36;
    /// `SZOMB`, the `p_stat` of a process that has exited and not been reaped.
    pub(super) const ZOMBIE: u8 = 5;
    /// `SRUN` and `SSLEEP`, the only two states a process asking about itself
    /// can be in. [`super::probe_kinfo_stride`] proves the `STAT` offset with
    /// them.
    pub(super) const RUNNING: u8 = 2;
    pub(super) const SLEEPING: u8 = 3;
    /// `kp_proc.p_pid`.
    pub(super) const PID: usize = 40;
    /// `kp_eproc.e_ppid`.
    pub(super) const PPID: usize = 560;
    /// The shortest entry all four reads fit inside.
    pub(super) const MIN_STRIDE: usize = PPID + 4;
}

/// The size of one `struct kinfo_proc` **as this kernel writes it**, or `None`
/// if the layout is not the one [`kinfo`] names.
///
/// Asked of the kernel rather than hardcoded: `sysctl(KERN_PROC_PID)` for a
/// single process answers with exactly one entry, so the byte count it returns
/// *is* the stride. **Every** offset the enumeration goes on to read is then
/// checked against a fact established some other way, which is what turns
/// "these offsets are documented" into "these offsets are right on the machine
/// we are running on".
///
/// All five, and not just the two that name the process, because the one that
/// would do the most damage is the one that is hardest to suspect. `STAT`
/// decides what gets **dropped**: if it pointed at the wrong byte, roughly one
/// entry in 256 would hold a 5 by chance, would be called a zombie, and would
/// vanish from a table still reporting itself complete - about three or four
/// live processes a pass, any one of which could be the worker. That is the
/// exact failure this probe exists to prevent, arriving through the probe's own
/// blind spot. So: this process is running, so its `p_stat` must read as `SRUN`
/// or `SSLEEP`; and its start token from the table must equal the one
/// `proc_pidinfo` reports for it, which pins both halves of the timeval exactly
/// rather than plausibly.
///
/// Cached on **success only**. A running kernel does not change its own struct
/// layout, so one success is good for the life of the process and saves two
/// syscalls a pass. A *failure* is not cached, and that asymmetry is the point:
/// the probe can fail for reasons that have nothing to do with the layout - a
/// `sysctl` refused under memory pressure, a parent that exits and is reaped
/// inside the window the two `getppid` calls leave open - and caching one of
/// those would take the detector dark for the rest of the session on the
/// strength of a single unlucky pass. A rare race may cost a pass; it may not
/// cost the session.
#[cfg(target_os = "macos")]
fn kinfo_stride() -> Option<usize> {
    // Zero is "not established yet": no `kinfo_proc` is zero bytes long, so the
    // sentinel cannot collide with a real answer.
    static STRIDE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let cached = STRIDE.load(std::sync::atomic::Ordering::Relaxed);
    if cached != 0 {
        return Some(cached);
    }
    let Some(stride) = probe_kinfo_stride() else {
        // Not cached, so this repeats every pass; a machine where it never
        // succeeds has a detector that says nothing at all, and the only way
        // anyone finds out is this line.
        log::debug!("kinfo_proc layout probe failed; the process table cannot be read this pass");
        return None;
    };
    STRIDE.store(stride, std::sync::atomic::Ordering::Relaxed);
    Some(stride)
}

#[cfg(target_os = "macos")]
fn probe_kinfo_stride() -> Option<usize> {
    // SAFETY: `getppid` takes nothing, cannot fail, and touches no memory.
    let parent_before = unsafe { libc::getppid() } as u32;
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        std::process::id() as libc::c_int,
    ];
    let mut buf = [0u8; 4096];
    let mut len = buf.len();
    // SAFETY: `mib` has the four elements the length argument names, `buf` has
    // `len` writable bytes, and the new-value pair is the documented null.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len < kinfo::MIN_STRIDE || len > buf.len() {
        return None;
    }
    let entry = &buf[..len];
    if read_u32(entry, kinfo::PID)? != std::process::id() {
        return None;
    }
    // Asked on both sides of the read, and either answer is accepted, because
    // the parent can exit while this runs: the process reparents to launchd and
    // `getppid` starts saying 1 for an entry that still names the old parent.
    // The two would disagree, the probe would fail a layout that is perfectly
    // right, and - because the answer is cached for the life of the process -
    // the detector would stay dark until the app was restarted. A rare race may
    // cost a pass; it may not cost the session.
    //
    // SAFETY: `getppid` takes nothing, cannot fail, and touches no memory.
    let parent_after = unsafe { libc::getppid() } as u32;
    let ppid = read_u32(entry, kinfo::PPID)?;
    if ppid != parent_before && ppid != parent_after {
        return None;
    }
    // This process is running, so the state byte has exactly two legal values.
    // A wrong `STAT` offset would have to land on one of them by chance.
    let stat = *entry.get(kinfo::STAT)?;
    if stat != kinfo::RUNNING && stat != kinfo::SLEEPING {
        return None;
    }
    // And the start offsets are pinned exactly, against the reading the old
    // enumeration used to take. `proc_pidinfo` is privilege-checked, which is
    // why it cannot enumerate - but this process is always allowed to ask about
    // itself, so it is a fine witness for one process.
    let info = libproc::libproc::proc_pid::pidinfo::<libproc::libproc::bsd_info::BSDInfo>(
        std::process::id() as i32,
        0,
    )
    .ok()?;
    if start_token_of(entry)? != (info.pbi_start_tvsec << 20) ^ info.pbi_start_tvusec {
        return None;
    }
    Some(len)
}

/// The whole process table as the kernel's own bytes.
///
/// Sized and then fetched, which is two calls and therefore a race: a process
/// starting in between makes the table outgrow the buffer and the fetch answers
/// `ENOMEM`. The slack covers the ordinary case and the retry covers the rest -
/// what must not happen is returning the short read, because a truncated table
/// is a table missing processes, which is the very thing this function exists
/// to stop.
#[cfg(target_os = "macos")]
fn kern_proc_all() -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL];
    for _ in 0..4 {
        let mut len = 0usize;
        // SAFETY: `mib` has the three elements named; a null `oldp` with a
        // valid `oldlenp` is the documented way to ask for the size.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return None;
        }
        // Room for processes started between the two calls.
        len += len / 4;
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` has `len` writable bytes and outlives the call; the
        // kernel writes at most `len` and reports what it wrote.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            buf.truncate(len);
            return Some(buf);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
            return None;
        }
    }
    None
}

/// A native-endian `u32` at `at`, or `None` if the entry is too short for one.
#[cfg(target_os = "macos")]
fn read_u32(entry: &[u8], at: usize) -> Option<u32> {
    let raw: [u8; 4] = entry.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_ne_bytes(raw))
}

/// A native-endian `u64` at `at`, or `None` if the entry is too short for one.
#[cfg(target_os = "macos")]
fn read_u64(entry: &[u8], at: usize) -> Option<u64> {
    let raw: [u8; 8] = entry.get(at..at + 8)?.try_into().ok()?;
    Some(u64::from_ne_bytes(raw))
}

/// One entry's pid, parent and start token, or `None` if it is not a live
/// process.
///
/// The start token is both halves of the start timeval, because seconds alone
/// collide: two processes a recycled pid apart can easily share a second. Every
/// reader of the table goes through here, so the table and the per-pid fallback
/// cannot give one process two different tokens - which would make it fail to
/// match itself in a baseline.
///
/// # A zombie is not a process this module reports on
///
/// `KERN_PROC_ALL` lists a process that has exited and not been reaped, and the
/// `proc_pidinfo` loop this replaced did not: it answered `ESRCH` for one and
/// dropped it. Measured on 7 September, every zombie on this machine was one of
/// those - 6 of 6 - and the app's own process had been holding several for
/// hours, so this is a lasting state and not a moment.
///
/// Carrying them into the table would be a defect introduced by widening it,
/// and precisely the one [`agent_under_pty`] warns about: a **zombie PTY
/// child** would have a readable identity, would therefore match its baseline
/// root, and would then present an empty subtree - `Absent`, which is "waiting
/// for you" in the rail for a pane whose shell is dead, held for
/// `TRUST_HORIZON`. A zombie holds nothing and runs nothing; it cannot be a
/// worker and it cannot be a live subtree's root, so it is left out and its
/// pid has no identity - [`Worker::Unknown`], which claims nothing.
#[cfg(target_os = "macos")]
fn live_entry(entry: &[u8]) -> Option<(u32, u32, u64)> {
    if *entry.get(kinfo::STAT)? == kinfo::ZOMBIE {
        return None;
    }
    let pid = read_u32(entry, kinfo::PID)?;
    if pid == 0 {
        return None;
    }
    Some((pid, read_u32(entry, kinfo::PPID)?, start_token_of(entry)?))
}

/// One entry's start token, both halves of the timeval - because seconds alone
/// collide, and two processes a recycled pid apart can easily share a second.
#[cfg(target_os = "macos")]
fn start_token_of(entry: &[u8]) -> Option<u64> {
    let sec = read_u64(entry, kinfo::START_SEC)?;
    let usec = u64::from(read_u32(entry, kinfo::START_USEC)?);
    Some((sec << 20) ^ usec)
}

/// The fallback for a pid asked about after the table was captured.
///
/// `sysctl` rather than `proc_pidinfo` for the reason the enumeration changed:
/// the per-pid `pidinfo` is privilege-checked and refuses another user's
/// process, which would make a root-owned descendant unnameable - and an
/// unnameable descendant costs the whole subtree its answer
/// ([`ProcessSnapshot::descendants_of`]). This path has no such check.
///
/// A pid that has gone answers zero bytes rather than an error, which the
/// length check below is what catches.
#[cfg(target_os = "macos")]
fn start_token_impl(pid: u32) -> Option<u64> {
    let stride = kinfo_stride()?;
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as libc::c_int,
    ];
    let mut buf = vec![0u8; stride];
    let mut len = stride;
    // SAFETY: `mib` has the four elements named and `buf` has `len` writable
    // bytes that outlive the call.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len < kinfo::MIN_STRIDE {
        return None;
    }
    // The kernel was asked about this pid; anything else means it is gone and
    // the buffer holds whatever the sizing left there.
    match live_entry(&buf[..len]) {
        Some((answered, _, start)) if answered == pid => Some(start),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn exe_path_impl(pid: u32) -> Option<PathBuf> {
    libproc::libproc::proc_pid::pidpath(pid as i32)
        .ok()
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// A Toolhelp snapshot hands back the whole table in one call, which is what
/// makes the per-pass capture the natural shape here too.
#[cfg(windows)]
fn capture_impl() -> Option<ProcessTable> {
    use std::mem;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: Win32 call; a successful snapshot handle is closed on every path
    // out of this function.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut entry: PROCESSENTRY32W = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    // SAFETY: `entry` is sized as the API requires and lives for the whole walk.
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) } != 0;
    while ok {
        if entry.th32ParentProcessID != entry.th32ProcessID {
            children
                .entry(entry.th32ParentProcessID)
                .or_default()
                .push(entry.th32ProcessID);
        }
        // SAFETY: same invariants as the first call.
        ok = unsafe { Process32NextW(snap, &mut entry) } != 0;
    }
    // A walk ends by `Process32NextW` answering zero, and the ordinary reason
    // is `ERROR_NO_MORE_FILES`. Any other reason stopped it **early**, leaving
    // out every process after that point - which is a truncated table, and must
    // not be reported as a whole one.
    //
    // SAFETY: read immediately after the call that set it, before anything else
    // can overwrite the thread's last-error value.
    let ended_cleanly = unsafe { GetLastError() } == ERROR_NO_MORE_FILES;
    // SAFETY: `snap` is a valid handle from a successful snapshot.
    unsafe { CloseHandle(snap) };
    // The Toolhelp walk carries no creation time, so the start-token map is
    // left empty and `identity_of` asks per pid instead. That is the one
    // platform where identity costs a call, and it is spent only on the
    // handful of processes in a pane's own subtree rather than on the table.
    //
    // Complete when the walk ran to its end: a `TH32CS_SNAPPROCESS` snapshot
    // enumerates every process on the machine, whatever its owner and whatever
    // its integrity level - the rights a caller may lack are for *opening* one,
    // which is `start_token_impl`'s problem and which `identity_of` answers by
    // refusing to name it. So no permission can drop a process from this table
    // the way the macOS `pidinfo` loop did; only a walk cut short can, and that
    // is what `ended_cleanly` carries.
    Some(ProcessTable {
        children,
        starts: HashMap::new(),
        complete: ended_cleanly,
    })
}

/// A process's creation time, which is what makes a recycled pid a different
/// identity. Windows recycles pids aggressively, so this matters more here
/// than anywhere.
///
/// Asked **after** the Toolhelp walk rather than during it, because
/// `PROCESSENTRY32W` does not carry a creation time and the alternative is
/// `NtQuerySystemInformation`. The gap is a real if narrow race: a pid that
/// exits and is reused between the walk and this call answers with the new
/// process's time. The result is a wrong identity, not a missing one - which
/// then fails to match the baseline and reads as [`Worker::Present`], the safe
/// direction. It can also bake a wrong identity **into** a baseline, and that
/// one is corrected at the next finished turn.
#[cfg(windows)]
fn start_token_impl(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: Win32 call; a non-null handle is closed on every path out.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited: FILETIME = unsafe { std::mem::zeroed() };
    let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
    let mut user: FILETIME = unsafe { std::mem::zeroed() };
    // SAFETY: `handle` is valid and all four out-params are owned locals.
    let ok =
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
    // SAFETY: `handle` came from a successful `OpenProcess`.
    unsafe { CloseHandle(handle) };
    if !ok {
        return None;
    }
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// Toolhelp's `szExeFile` is a basename, which is exactly the field that cannot
/// tell our shim from the agent it fronts for - both are called `claude.exe`.
/// `QueryFullProcessImageNameW` is the one that answers with a path.
#[cfg(windows)]
fn exe_path_impl(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    // SAFETY: Win32 call; a non-null handle is closed on every path out.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buf = [0u16; 32768];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` has `len` writable u16s; the API writes at most that many
    // and updates `len` to the written length.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) } != 0;
    // SAFETY: `handle` came from a successful `OpenProcess`.
    unsafe { CloseHandle(handle) };
    if !ok {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(
        &buf[..len as usize],
    )))
}

// ---------------------------------------------------------------------------
// Everywhere else
// ---------------------------------------------------------------------------

/// The question cannot be asked, and saying so is the whole contract: the rule
/// reads `Unknown` as no evidence and never reports a person waiting on the
/// strength of it.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn capture_impl() -> Option<ProcessTable> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn start_token_impl(_pid: u32) -> Option<u64> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn exe_path_impl(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests below assert about the **test process's own** children, which
    /// is state every test in this binary shares. Cargo runs them on threads of
    /// one process, so a test that spawns would otherwise be visible to a test
    /// that counts.
    static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Pid zero is not a process to ask about, and on some platforms it names a
    /// whole process group - a different question with a different answer.
    #[test]
    fn pid_zero_is_never_answered() {
        let snap = ProcessSnapshot::capture().expect("this platform can answer");
        assert_eq!(snap.worker_under(0), Worker::Unknown);
        assert_eq!(exe_path_of(0), None);
    }

    /// The smoke test that the platform arm is wired at all - a stub returning
    /// `Unknown` and `None` everywhere would still pass every other test here.
    ///
    /// The executable half is the whole discriminator: it has to answer with a
    /// **path**, because the shim and the agent it fronts for share a name and
    /// only the path can separate them.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn a_platform_that_can_answer_gives_a_real_answer() {
        let me = std::process::id();
        let snap = ProcessSnapshot::capture().expect("snapshot");
        assert_ne!(
            snap.worker_under(me),
            Worker::Unknown,
            "this platform is supposed to be able to walk its own process tree"
        );
        let exe = exe_path_of(me).expect("this platform can name its own executable");
        assert!(
            exe.is_absolute(),
            "a basename cannot tell a shim from an agent"
        );
        assert_eq!(
            exe.canonicalize().ok(),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok()),
            "the path must be the running executable's own"
        );
    }

    /// A child that exists is seen, and its disappearance is seen too. Written
    /// as one test because the second half is only meaningful after the first.
    #[test]
    #[cfg(unix)]
    fn a_spawned_child_is_seen_and_then_is_not() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn");
        let me = std::process::id();
        let snap = ProcessSnapshot::capture().expect("snapshot");
        assert_eq!(snap.worker_under(me), Worker::Present);

        child.kill().expect("kill");
        child.wait().expect("reap");
        let snap = ProcessSnapshot::capture().expect("snapshot");
        assert_eq!(
            snap.worker_under(me),
            Worker::Absent,
            "a reaped child must stop counting - a zombie left behind would \
             keep every session looking busy"
        );
    }

    /// A spawned fixture that reaps itself, **including on a panic**.
    ///
    /// Without this a failing assertion leaves a child - or worse a zombie,
    /// which every platform still lists with its parent - hanging off the test
    /// process, and the next test to count our children fails for a reason that
    /// has nothing to do with it.
    #[cfg(unix)]
    struct Spawned(std::process::Child);

    #[cfg(unix)]
    impl Drop for Spawned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Spawn a real `shell → middle → worker` chain and hand back its pids,
    /// once the third level exists. The shape is the measured one: a pane's PTY
    /// child is the user's shell, the shim sits under it, and the agent sits
    /// under the shim.
    ///
    /// `; true` at both levels is load-bearing - without a second command
    /// `sh -c` execs its only one and collapses the level under test.
    #[cfg(unix)]
    fn spawn_three_deep() -> (Spawned, u32, u32) {
        let shell = Spawned(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("/bin/sh -c 'sleep 30; true'; true")
                .spawn()
                .expect("spawn the shell"),
        );
        let shell_pid = shell.0.id();
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            let Some(snap) = ProcessSnapshot::capture() else {
                continue;
            };
            if let [middle] = *snap.children_of(shell_pid)
                && !snap.children_of(middle).is_empty()
            {
                return (shell, shell_pid, middle);
            }
        }
        panic!("the three-deep chain never appeared");
    }

    /// The blocker this module was extended for, on a real process tree: asking
    /// about the middle of the chain - which is what `Thread::agent_pid` names,
    /// because `SPLITLANE_AI_PID` is the shim's own pid - answers `Present`
    /// while the level below it is idle. That is the silent failure: the
    /// `Spawns` arm of the rule would never fire.
    ///
    /// Stepping over it answers `Absent`, which is what lets the rule speak.
    #[test]
    #[cfg(unix)]
    fn the_agent_is_found_by_stepping_over_the_shim() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let (_shell, shell_pid, shim_pid) = spawn_three_deep();
        let snap = ProcessSnapshot::capture().expect("snapshot");

        let agent = agent_under_pty_by(&snap, shell_pid, &|pid| pid == shim_pid)
            .expect("the agent under the shim");
        assert_ne!(agent, shim_pid, "the shim is not the agent");
        assert_eq!(
            snap.worker_under(shim_pid),
            Worker::Present,
            "the shim always has a child - this is why its own pid cannot be asked"
        );
        assert_eq!(
            snap.worker_under(agent),
            Worker::Absent,
            "the resolved agent is idle, which is what the rule needs to see"
        );
    }

    /// Identity is pid **plus** start, and that is what makes a recycled pid a
    /// different process rather than the same one. Without it the baseline
    /// below would hide a worker that inherited a dead process's number.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn identity_is_more_than_a_pid() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let snap = ProcessSnapshot::capture().expect("snapshot");
        let me = std::process::id();
        let (pid, start) = snap.identity_of(me).expect("our own identity");
        assert_eq!(pid, me);
        assert_ne!(start, 0, "a start token of zero would collapse identities");
        // Stable across snapshots: an identity that changed every pass would
        // make every process look new, and every process looks like a worker.
        let again = ProcessSnapshot::capture().expect("snapshot");
        assert_eq!(again.identity_of(me), Some((pid, start)));
        assert_eq!(snap.identity_of(0), None, "pid zero is nobody");
    }

    /// The whole point, on a real process tree: a child that was there when the
    /// turn ended is structural and does not count, and one that appeared after
    /// it does. This is what a stdio MCP server and a `Bash` command look like.
    #[test]
    #[cfg(unix)]
    fn a_baseline_tells_a_worker_from_the_furniture() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        // Stands in for the shim/agent/MCP-server furniture: present before the
        // baseline is taken, and still present after.
        let furniture = Spawned(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("sleep 30; true")
                .spawn()
                .expect("spawn the furniture"),
        );
        let me = std::process::id();

        // Wait for the furniture to be WHOLE, not merely started. `sh -c "sleep
        // 30; true"` is two forks and not one: the shell appears at once, and
        // forks `sleep` a moment later. A baseline taken in between records the
        // shell without the grandchild, so the grandchild then looks like a
        // process that appeared after the turn ended - the exact thing this
        // test asserts does not happen. A fast machine loses that race rarely
        // and an emulated aarch64 runner loses it often, which is how it
        // reached CI as a flake rather than as a failure.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let snap = loop {
            let snap = ProcessSnapshot::capture().expect("snapshot");
            if !snap.children_of(furniture.0.id()).is_empty() {
                break snap;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the furniture never forked its `sleep` child"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(
            !snap.children_of(me).is_empty(),
            "the furniture must be up before the baseline is taken"
        );
        let baseline = WorkerBaseline::take(&snap, me).expect("baseline");

        // Furniture alone: the old question would say `Present` here for ever,
        // which is exactly the failure this replaces.
        assert_eq!(snap.worker_under(me), Worker::Present);
        let snap = ProcessSnapshot::capture().expect("snapshot");
        assert_eq!(
            worker_against(&snap, Some(&baseline), me),
            Worker::Absent,
            "everything under this pane was here when the turn ended"
        );

        // A command starts.
        let _worker = Spawned(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("sleep 30; true")
                .spawn()
                .expect("spawn the worker"),
        );
        let snap = ProcessSnapshot::capture().expect("snapshot");
        assert_eq!(
            worker_against(&snap, Some(&baseline), me),
            Worker::Present,
            "a process that was not in the baseline is a worker"
        );
    }

    /// Every uncertainty has to answer `Unknown`, because `Absent` is the only
    /// value that can put "a person is being kept waiting" in the rail.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn nothing_is_claimed_without_a_baseline_or_a_live_root() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let snap = ProcessSnapshot::capture().expect("snapshot");
        let me = std::process::id();

        // No turn has ended yet: the resting shape is simply not known.
        assert_eq!(worker_against(&snap, None, me), Worker::Unknown);

        // A baseline belonging to a different root - a remounted pane, or a
        // recycled pid landing on this number - describes somebody else.
        let baseline = WorkerBaseline::take(&snap, me).expect("baseline");
        let impostor = WorkerBaseline {
            root: (me, baseline.root.1.wrapping_add(1)),
            structural: baseline.structural.clone(),
        };
        assert_eq!(worker_against(&snap, Some(&impostor), me), Worker::Unknown);

        // Not a process at all.
        assert_eq!(worker_against(&snap, Some(&baseline), 0), Worker::Unknown);
        assert_eq!(WorkerBaseline::take(&snap, 0), None);
        assert_eq!(snap.descendants_of(0), None);
    }

    /// A shape this build has not measured gets no answer at all rather than a
    /// guess - and `None` is read by every caller as [`Worker::Unknown`], which
    /// claims nothing.
    #[test]
    #[cfg(unix)]
    fn an_unrecognised_shape_answers_nothing() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let (_shell, shell_pid, shim_pid) = spawn_three_deep();
        let snap = ProcessSnapshot::capture().expect("snapshot");

        // Nothing of ours under this PTY: the user started the agent some other
        // way, or started none.
        assert_eq!(agent_under_pty_by(&snap, shell_pid, &|_| false), None);
        // A shim with no child of its own: still spawning, or its agent has
        // already exited.
        let worker = snap.children_of(shim_pid)[0];
        assert_eq!(
            agent_under_pty_by(&snap, shell_pid, &|pid| pid == worker),
            None
        );
        // A pid that is not a process.
        assert_eq!(agent_under_pty_by(&snap, 0, &|_| true), None);
    }

    /// Two agents started from one shell: both are ours, and nothing in the
    /// tree says which surface asked, so neither is claimed.
    #[test]
    #[cfg(unix)]
    fn two_shims_under_one_pty_answer_nothing() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let shell = Spawned(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("/bin/sh -c 'sleep 30; true' & /bin/sh -c 'sleep 30; true' & wait")
                .spawn()
                .expect("spawn the shell"),
        );
        let shell_pid = shell.0.id();
        let siblings = (0..200)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let snap = ProcessSnapshot::capture()?;
                let kids = snap.children_of(shell_pid).to_vec();
                (kids.len() == 2).then_some(kids)
            })
            .expect("two children of the shell");
        let snap = ProcessSnapshot::capture().expect("snapshot");
        assert_eq!(
            agent_under_pty_by(&snap, shell_pid, &|pid| siblings.contains(&pid)),
            None
        );
    }

    /// The other half - that a staged shim is recognised **by its executable
    /// path** - against the real binary, because nothing smaller can stand in
    /// for it: a copied system binary is SIGKILLed on exec by macOS, and a
    /// shell script reports its interpreter's path rather than its own.
    ///
    /// Ignored by default: it stages this build's shim into the user's cache
    /// dir and runs it. Run it after touching either the extraction plan or the
    /// per-OS `exe_path_impl`.
    ///
    /// ```text
    /// cargo test -p splitlane-app --bin splitlane -- --ignored the_agent_is_found_under_the_real_shim --nocapture
    /// ```
    #[test]
    #[ignore = "stages and runs this build's shim binary"]
    #[cfg(unix)]
    fn the_agent_is_found_under_the_real_shim() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let shim_dir =
            crate::ai_hooks::extract::ensure_binaries_extracted().expect("stage the shim binaries");
        let shim_dir = shim_dir.canonicalize().unwrap_or(shim_dir);

        // The shim PATH-walks for the real agent, excluding its own directory.
        // A script is a perfectly good stand-in for that agent: the executable
        // path is only ever asked of the shim.
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("claude");
        std::fs::write(&real, "#!/bin/sh\nsleep 30\n").expect("write");
        std::fs::set_permissions(
            &real,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod");

        let path = format!(
            "{}:{}:/bin:/usr/bin",
            shim_dir.display(),
            dir.path().display()
        );
        let shell = Spawned(
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("claude; true")
                .env("PATH", path)
                .env("SPLITLANE_BIN_DIR", shim_dir.display().to_string())
                .current_dir(dir.path())
                .spawn()
                .expect("spawn the shell"),
        );
        let shell_pid = shell.0.id();

        let agent = (0..200)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let snap = ProcessSnapshot::capture()?;
                agent_under_pty(&snap, shell_pid, &shim_dir)
            })
            .expect("the agent under the real shim");
        // The shim was found by its path alone: it is called `claude`, exactly
        // like the binary it fronts for, and it is the shell's only child.
        let snap = ProcessSnapshot::capture().expect("snapshot");
        let [shim_pid] = *snap.children_of(shell_pid) else {
            panic!("the shim is the shell's only child");
        };
        assert_eq!(snap.children_of(shim_pid), &[agent]);
        assert!(exe_path_of(shim_pid).is_some_and(|exe| exe.starts_with(&shim_dir)));
    }

    /// The hole this module carried until 7 September: a process the platform
    /// declined to *list* never became anybody's child, so a root-owned worker
    /// inside a pane's own subtree read as no worker at all and the rule
    /// reported a person waiting while the command ran.
    ///
    /// Pid 1 is the anchor because it is root's on every ordinary system and
    /// this process is not root. Measured on macOS on 7 September, the
    /// `proc_pidinfo` call the old enumeration was built on answers `EPERM` for
    /// it - so under the old code this assertion failed, and it is the whole of
    /// what changed.
    ///
    /// Skipped where the table itself says it is not whole, which is the honest
    /// state under Linux `hidepid` and is asserted separately below.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_process_this_user_does_not_own_is_still_named() {
        let snap = ProcessSnapshot::capture().expect("snapshot");
        if !snap.complete() {
            // A `hidepid` mount, and the flag is the point of the next test.
            return;
        }
        assert!(
            snap.identity_of(1).is_some(),
            "a complete table must be able to name pid 1, which this user does not own"
        );
    }

    /// A reading that missed a process may not produce the one answer that
    /// claims something about a person.
    ///
    /// `Absent` means "every process under this pane was here when the turn
    /// ended", and a process the walk never saw is one the comparison cannot
    /// miss the absence of. The whole table therefore disqualifies itself, and
    /// it has to happen before the per-subtree reasoning rather than inside it.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn an_incomplete_table_never_reports_a_person_waiting() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let me = std::process::id();
        let snap = ProcessSnapshot::capture().expect("snapshot");
        let baseline = WorkerBaseline::take(&snap, me).expect("baseline");

        // Nothing has started since the baseline, so a whole reading says so.
        assert_eq!(
            worker_against(&snap, Some(&baseline), me),
            Worker::Absent,
            "a complete reading of an unchanged subtree is the one shape that yields Absent"
        );

        // The same subtree, read off a table that admits it missed something.
        assert_eq!(
            worker_against(&snap.into_incomplete(), Some(&baseline), me),
            Worker::Unknown,
            "an unlisted process could be the worker, and nothing here would notice"
        );
    }

    /// The offsets in [`kinfo`] are an ABI rather than a guess, and this is
    /// where that stops being taken on trust.
    ///
    /// Three things are checked against facts established some other way: the
    /// kernel's own entry size divides the table exactly, the entry for this
    /// process names this process's parent, and the start token read out of the
    /// table is the one `proc_pidinfo` reports for the same process. The last
    /// is what makes this the *same* reading the old enumeration produced
    /// rather than a differently-shaped one - the table and the per-pid
    /// fallback both feed `identity_of`, and a process that got two different
    /// tokens would fail to match itself in a baseline.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_kinfo_layout_is_the_one_this_build_names() {
        use libproc::libproc::bsd_info::BSDInfo;
        use libproc::libproc::proc_pid::pidinfo;

        let stride = kinfo_stride().expect("the kernel's kinfo_proc layout");
        let table = kern_proc_all().expect("the process table");
        assert_eq!(
            table.len() % stride,
            0,
            "the table is a whole number of entries of the kernel's own size"
        );

        let me = std::process::id();
        // SAFETY: `getppid` takes nothing, cannot fail, and touches no memory.
        let parent = unsafe { libc::getppid() } as u32;
        let (_, ppid, token) = table
            .chunks_exact(stride)
            .filter_map(live_entry)
            .find(|&(pid, _, _)| pid == me)
            .expect("this process is in the table");
        assert_eq!(ppid, parent);

        let info = pidinfo::<BSDInfo>(me as i32, 0).expect("pidinfo for our own process");
        assert_eq!(
            token,
            (info.pbi_start_tvsec << 20) ^ info.pbi_start_tvusec,
            "the table and proc_pidinfo report the same start for the same process"
        );
        assert_eq!(
            start_token_impl(me),
            Some(token),
            "the per-pid fallback agrees with the table it falls back from"
        );
    }

    /// A process that has exited and not been reaped is not a process this
    /// module reports on.
    ///
    /// It matters because of what a **zombie PTY child** would otherwise do:
    /// have a readable identity, match its baseline root, present an empty
    /// subtree, and so reach `Absent` - "waiting for you" in the rail for a
    /// pane whose shell is dead, held for `TRUST_HORIZON`. macOS's old
    /// `proc_pidinfo` enumeration excluded zombies by accident, because it
    /// answers `ESRCH` for one; `sysctl(KERN_PROC_ALL)` lists them, and `/proc`
    /// always did, so the rule is stated on both rather than inherited from
    /// either.
    #[test]
    #[cfg(unix)]
    fn a_zombie_is_not_a_process_this_module_reports_on() {
        let _guard = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        // Rust does not reap on drop, so this child stays a zombie until the
        // explicit `wait` at the end of the test.
        let mut dead = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn a child that exits at once");
        let zombie = dead.id();
        let me = std::process::id();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let snap = ProcessSnapshot::capture().expect("snapshot");
            if snap.identity_of(zombie).is_none() {
                assert!(
                    !snap.children_of(me).contains(&zombie),
                    "a zombie must not be anybody's child either"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child never became a zombie this build declines to name"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        dead.wait().expect("reap the zombie");
    }

    /// The `/proc/<pid>/stat` field walk, and the three answers it has to keep
    /// apart. Runs on Linux only, which is where the code it covers compiles.
    ///
    /// The last assertion is the one worth having: a zombie and a line this
    /// build did not understand both mean "no facts from here", and if the
    /// walk conflated them, every machine with a single zombie on it would mark
    /// its process table incomplete and lose "waiting for you" altogether.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_stat_walk_tells_live_from_zombie_from_unreadable() {
        // A real line's shape, with a comm holding both a space and parens, so
        // the `rfind(')')` split is exercised the way the kernel can produce it.
        let comm = "(my (weird) proc)";
        let mut fields: Vec<String> = vec!["4242".into(), comm.into(), "S".into(), "1000".into()];
        for filler in 5..=21 {
            fields.push(filler.to_string());
        }
        fields.push("987654321".into()); // field 22, starttime
        for filler in 23..=52 {
            fields.push(filler.to_string());
        }
        let line = fields.join(" ") + "\n";

        match parse_stat(&line) {
            ProcStat::Live { ppid, start } => {
                assert_eq!(ppid, 1000, "ppid is field 4");
                assert_eq!(start, 987_654_321, "starttime is field 22");
            }
            _ => panic!("a well-formed line is Live"),
        }

        let zombie = line.replacen(" S ", " Z ", 1);
        assert!(matches!(parse_stat(&zombie), ProcStat::Zombie));

        // Listed and readable, but not understood - which removes a process
        // from the table exactly as `hidepid` does, and must be admitted.
        assert!(matches!(
            parse_stat(&format!("4242 {comm} S 1000 5 6")),
            ProcStat::Unreadable
        ));
        assert!(matches!(
            parse_stat("garbage with no paren"),
            ProcStat::Unreadable
        ));
        assert!(matches!(
            parse_stat(&format!("4242 {comm} S notanumber 5")),
            ProcStat::Unreadable
        ));

        // And the distinction that makes the three-valued answer necessary.
        assert!(!matches!(parse_stat(&zombie), ProcStat::Unreadable));
    }
}
