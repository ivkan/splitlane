//! Event-subscription callbacks and background workers for `SplitlaneApp`.
//!
//! Hosts the GPUI `subscribe` handlers (`handle_title_bar_event`,
//! `handle_pane_event`, `handle_terminal_event`) plus the port-scan /
//! loader-animation / stale-PID-sweep workers and the CWD change handler.
//!
//! Extracted from `main.rs` - pure code-motion, behaviour unchanged.

use gpui::{App, AppContext, Context, Entity, Window};
use notify::Watcher;
use splitlane_config::schema::TerminalSurfaceProfile;

use crate::layout::{LayoutTree, MAX_PANES};
use crate::pane::{self, Pane};
use crate::pane_drag::DropEdge;
use crate::terminal::{self, TerminalView};
use crate::window_chrome::title_bar;
use crate::{SplitlaneApp, ai_types};

/// Cross-platform "is this PID still running?" probe used by the AI agent
/// stale-PID sweep. Unix path preserves the original `kill(pid, 0)` +
/// `ESRCH` semantics (EPERM ⇒ alive). Windows path mirrors the pattern in
/// `terminal::pty_session::Drop`: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
/// returns NULL for a dead/inaccessible PID. This keeps `libc::` calls
/// off the Windows compile path.
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid > i32::MAX as u32 {
            return false;
        }
        // SAFETY: `libc::kill` with sig=0 performs error-checking only and
        // does not deliver a signal. The call takes an i32 pid by value and
        // has no memory aliasing requirements.
        let ret = unsafe { libc::kill(pid as i32, 0) };
        if ret == -1 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            // ESRCH = no such process; EPERM/etc. ⇒ process exists but we
            // can't signal it - keep the entry.
            return errno != libc::ESRCH;
        }
        true
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::OpenProcess;
        // PROCESS_QUERY_LIMITED_INFORMATION (winnt.h: 0x1000) - minimum
        // access right that lets OpenProcess succeed for any visible PID.
        // Declared locally so we don't require an extra windows-sys feature.
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        if pid == 0 {
            return false;
        }
        // SAFETY: `OpenProcess` either returns a valid handle that we close,
        // or NULL. No memory aliasing. `CloseHandle` on a valid handle is
        // always sound.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let _ = CloseHandle(handle);
            true
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Conservative fallback for exotic targets: never sweep. Better to
        // keep a stale entry than to drop a live one and confuse the AI
        // badge state.
        let _ = pid;
        true
    }
}

fn split_pane_at_edge(
    root: &mut LayoutTree,
    target: &Entity<Pane>,
    edge: DropEdge,
    new_pane: Entity<Pane>,
) -> bool {
    let (direction, swap) = edge.to_split();
    if !root.split_at_pane(target, direction, new_pane.clone()) {
        return false;
    }
    if swap {
        root.swap_panes(target, &new_pane);
    }
    true
}

/// Parse the `starttime` field (22) from `/proc/{pid}/stat` content. The
/// comm field (2) is parenthesized and may contain spaces and parens
/// (`(tmux: server)`, `(next-server (v15))`) - split after the LAST `)`
/// (kernel-guaranteed unambiguous), then take the 20th whitespace field of
/// the remainder (state is field 3 → index 0, so starttime is index 19).
/// Platform-neutral pure parsing so the fixture test runs on every host.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat_starttime(stat: &str) -> Option<u64> {
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(19)?.parse::<u64>().ok()
}

/// OS start time of a process, as an opaque value only ever compared for
/// equality. Pinned on `AgentSession` at creation and re-probed by the
/// sweep to detect PID reuse (a recycled PID passes `pid_is_alive` but
/// carries a different start time). `None` on probe failure (EPERM, dead
/// process, exotic target) - callers fall back to liveness-only.
#[cfg(target_os = "linux")]
pub(crate) fn pid_start_time(pid: u32) -> Option<u64> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_proc_stat_starttime(&content)
}

#[cfg(target_os = "macos")]
pub(crate) fn pid_start_time(pid: u32) -> Option<u64> {
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::proc_pid::pidinfo;
    // EPERM (SIP-protected targets) and dead-pid races degrade to None -
    // the caller keeps the conservative liveness-only check.
    let info = pidinfo::<BSDInfo>(pid as i32, 0).ok()?;
    Some(
        info.pbi_start_tvsec
            .wrapping_mul(1_000_000)
            .wrapping_add(info.pbi_start_tvusec),
    )
}

#[cfg(windows)]
pub(crate) fn pid_start_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess};
    // Same minimal access right as `pid_is_alive` above.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    if pid == 0 {
        return None;
    }
    // SAFETY: `OpenProcess` returns a valid handle (closed below) or NULL;
    // `GetProcessTimes` writes only into the four provided FILETIMEs, which
    // are plain-old-data and fully initialized by `zeroed`.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn pid_start_time(_pid: u32) -> Option<u64> {
    None
}

/// [`pid_is_alive`] hardened against PID reuse: when the session pinned a
/// start time at creation, a live PID with a DIFFERENT current start time
/// is a recycled PID - the original agent is gone. An unknown start time on
/// either side keeps the conservative "alive" answer.
fn pid_matches(pid: u32, pinned_start: Option<u64>) -> bool {
    if !pid_is_alive(pid) {
        return false;
    }
    match (pinned_start, pid_start_time(pid)) {
        (Some(pinned), Some(current)) => pinned == current,
        _ => true,
    }
}

fn keep_session_after_surface_purge(
    dying_surface_id: u64,
    pid: u32,
    session: &ai_types::AgentSession,
) -> bool {
    if session.surface_id == Some(dying_surface_id) {
        return false;
    }
    session.surface_id.is_some() || pid > i32::MAX as u32 || pid_matches(pid, session.proc_start)
}

fn stale_sweep_keeps_without_pid_probe(
    pid: u32,
    session: &ai_types::AgentSession,
    live_surfaces: &std::collections::HashSet<u64>,
) -> bool {
    pid > i32::MAX as u32
        || (session.state == ai_types::AgentState::Errored
            && session
                .surface_id
                .is_some_and(|sid| live_surfaces.contains(&sid)))
}

fn merge_service_label(
    labels: &mut std::collections::HashMap<u16, crate::terminal::ServiceInfo>,
    info: crate::terminal::ServiceInfo,
) -> bool {
    if let Some(existing) = labels.get(&info.port)
        && existing.is_frontend
        && !info.is_frontend
    {
        return false;
    }
    if labels.get(&info.port) == Some(&info) {
        return false;
    }
    labels.insert(info.port, info);
    true
}

fn scan_workspace_ports(
    scan: &std::collections::HashMap<u64, crate::workspace::PaneScan>,
) -> Vec<u16> {
    let mut ports: Vec<u16> = scan
        .values()
        .flat_map(|s| s.ports.iter().map(|e| e.port))
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn scan_detected_agents(
    scan: &std::collections::HashMap<u64, crate::workspace::PaneScan>,
) -> std::collections::HashSet<String> {
    scan.values()
        .flat_map(|s| s.agents.iter().cloned())
        .collect()
}

fn merge_frontend_scan_labels(
    labels: &mut std::collections::HashMap<u16, crate::terminal::ServiceInfo>,
    scan: &std::collections::HashMap<u64, crate::workspace::PaneScan>,
) -> bool {
    let mut changed = false;
    for entry in scan.values().flat_map(|s| s.ports.iter()) {
        let Some(label) = entry.frontend else {
            continue;
        };
        let fallback_url = || format!("http://localhost:{}", entry.port);
        match labels.entry(entry.port) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let info = e.get_mut();
                if !info.is_frontend {
                    info.is_frontend = true;
                    info.label = Some(label.to_string());
                    if info.url.is_none() {
                        info.url = Some(fallback_url());
                    }
                    changed = true;
                    continue;
                }
                if info.label.is_none() {
                    info.label = Some(label.to_string());
                    changed = true;
                }
                if info.url.is_none() {
                    info.url = Some(fallback_url());
                    changed = true;
                }
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(crate::terminal::ServiceInfo {
                    port: entry.port,
                    url: Some(fallback_url()),
                    label: Some(label.to_string()),
                    is_frontend: true,
                });
                changed = true;
            }
        }
    }
    changed
}

fn merge_scan_workspace_state(
    active_ports: &mut Vec<u16>,
    service_labels: &mut std::collections::HashMap<u16, crate::terminal::ServiceInfo>,
    detected_agents: &mut std::collections::HashSet<String>,
    scan: &std::collections::HashMap<u64, crate::workspace::PaneScan>,
) -> bool {
    let ports = scan_workspace_ports(scan);
    let next_agents = scan_detected_agents(scan);
    let mut changed = false;

    if *active_ports != ports {
        *active_ports = ports;
        changed = true;
    }
    let before = service_labels.len();
    service_labels.retain(|port, _| active_ports.contains(port));
    if service_labels.len() != before {
        changed = true;
    }
    let frontend_ports: std::collections::HashSet<u16> = scan
        .values()
        .flat_map(|s| s.ports.iter())
        .filter(|entry| entry.frontend.is_some())
        .map(|entry| entry.port)
        .collect();
    for info in service_labels.values_mut() {
        if info.is_frontend && !frontend_ports.contains(&info.port) {
            info.is_frontend = false;
            changed = true;
        }
    }
    if *detected_agents != next_agents {
        *detected_agents = next_agents;
        changed = true;
    }
    merge_frontend_scan_labels(service_labels, scan) || changed
}

fn port_ownership(
    scan: &std::collections::HashMap<u64, crate::workspace::PaneScan>,
) -> (
    std::collections::HashMap<u16, u64>,
    std::collections::HashSet<u16>,
) {
    let mut owner = std::collections::HashMap::new();
    let mut shared = std::collections::HashSet::new();
    for (tid, s) in scan {
        for e in &s.ports {
            match owner.entry(e.port) {
                std::collections::hash_map::Entry::Occupied(o) => {
                    if *o.get() != *tid {
                        shared.insert(e.port);
                    }
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(*tid);
                }
            }
        }
    }
    (owner, shared)
}

fn announced_port_conflicts(
    announced_ports: &[u16],
    tid: u64,
    owner: &std::collections::HashMap<u16, u64>,
    shared: &std::collections::HashSet<u16>,
    display_names: &std::collections::HashMap<u64, String>,
) -> Vec<(u16, String)> {
    announced_ports
        .iter()
        .filter_map(|p| match owner.get(p) {
            Some(&o) if o != tid && !shared.contains(p) => {
                Some((*p, display_names.get(&o).cloned().unwrap_or_default()))
            }
            _ => None,
        })
        .collect()
}

impl SplitlaneApp {
    pub(crate) fn handle_title_bar_event(
        &mut self,
        _title_bar: Entity<title_bar::TitleBar>,
        event: &title_bar::TitleBarEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            title_bar::TitleBarEvent::CloseRequested => {
                self.request_exit(crate::app::exit_guard::ExitIntent::Quit, cx);
            }
            title_bar::TitleBarEvent::ToggleSidebar => {
                self.toggle_primary_sidebar(cx);
                if !self.primary_sidebar_visible {
                    self.dismiss_transient_surfaces();
                }
            }
            // Deferred to `render`, which has the `Window` that focusing the
            // popover needs.
            title_bar::TitleBarEvent::WaitingChipClicked => {
                self.pending_waiting_chip = true;
                cx.notify();
            }
        }
    }

    /// Drop `pane` out of whichever container holds it, in the tree or in the
    /// layout a zoom put aside, and never leave that container with no pane at
    /// all.
    ///
    /// Extracted because two events end here: a pane that emptied itself
    /// (`Remove`) and a slot the user closed (`CloseSlot`). They differ only
    /// in whether the closing is recorded for undo.
    pub(crate) fn drop_pane_from_layout(&mut self, pane: &Entity<Pane>, cx: &mut Context<Self>) {
        // Not necessarily the active container - shells exit in background
        // ones too.
        let Some(ws_idx) = self.workspaces.iter().position(|ws| ws.contains_pane(pane)) else {
            return;
        };

        // Where the closed slot sat, counted the way the rail counts them.
        // Focus goes to whatever takes that index afterwards (`FOCUS.md`), so
        // the eye stays where the slot was.
        let closed_leaf_idx = self.workspaces[ws_idx]
            .root
            .as_ref()
            .and_then(|root| root.collect_leaves().iter().position(|leaf| leaf == pane));
        let root_contains = self.workspaces[ws_idx]
            .root
            .as_ref()
            .is_some_and(|root| root.contains_leaf(pane));
        let saved_contains = self.workspaces[ws_idx]
            .saved_layout
            .as_ref()
            .is_some_and(|saved| saved.contains_leaf(pane));

        // In a grid the cell stays and the launcher moves into it. A grid has
        // exactly four cells, so removing one would leave a shape that is none
        // of the three forms - and collapsing to a 2-over-1 would be the layout
        // rearranging itself unasked, which the grid's design forbids outright. Emptying
        // the cell is the same answer `Add pane` gives: an empty pane is an
        // invitation, not a hole.
        //
        // Asked of the tree that actually holds the pane, because a zoomed
        // container keeps its real layout in `saved_layout` and shows one leaf.
        let grid_cell = if saved_contains {
            self.workspaces[ws_idx]
                .saved_layout
                .as_ref()
                .is_some_and(|saved| saved.is_grid())
        } else {
            self.workspaces[ws_idx]
                .root
                .as_ref()
                .is_some_and(|root| root.is_grid())
        };
        if grid_cell {
            let launcher = self.create_pane_with_existing_tabs(Vec::new(), 0, cx);
            if saved_contains {
                if let Some(saved) = self.workspaces[ws_idx].saved_layout.as_mut() {
                    saved.replace_leaf(pane, launcher);
                }
                // The pane being closed is also the one the zoom is showing, so
                // `root` is a `Leaf` holding it: repairing only `saved_layout`
                // would leave the screen on a pane that no longer exists. The
                // removal path below makes exactly this move for exactly this
                // reason - the grid comes back out of the zoom. Without it the
                // two close doors disagreed, which is what a cross-vendor pass
                // found.
                if root_contains && let Some(saved) = self.workspaces[ws_idx].saved_layout.take() {
                    self.workspaces[ws_idx].root = Some(saved);
                }
            } else if let Some(root) = self.workspaces[ws_idx].root.as_mut() {
                root.replace_leaf(pane, launcher);
            }
        } else if saved_contains {
            if let Some(saved) = self.workspaces[ws_idx].saved_layout.take() {
                let (new_saved, _) = saved.remove_pane(pane);
                if root_contains {
                    self.workspaces[ws_idx].root = new_saved;
                } else {
                    self.workspaces[ws_idx].saved_layout = new_saved;
                }
            }
        } else if let Some(root) = self.workspaces[ws_idx].root.take() {
            let (new_root, _) = root.remove_pane(pane);
            self.workspaces[ws_idx].root = new_root;
        }

        // Closing the last pane leaves the container with none, and that is
        // the end of it. Two earlier answers were both wrong: respawning a
        // shell made "Close pane" look like it had done nothing while quietly
        // ending a process, and putting the launcher there left an empty pane
        // standing - a second door to what the rail row's own
        // `+ agent` / `+ shell` / `+ worktree` already offer.
        //
        // `target_pane_for` has always had the arm for this ("a container
        // with no tree at all ... emptied by closing its last pane"), so the
        // next surface opened into this container becomes its first pane.
        // The closed slot may have been the one taking input, and this path
        // has no `Window` to hand focus on with - so aim the deferred channel
        // at a surviving slot. Without it the window is left with focus on a
        // pane that no longer exists: the next keystroke goes nowhere, and the
        // three focus signals correctly report that nothing is focused, which
        // is an honest answer to a question that should not have been asked.
        // `⌘⇧W` already does this; the header's `×` did not.
        let closed_focused_pane = self
            .focused_pane_now
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .is_some_and(|focused| focused == *pane);
        if closed_focused_pane {
            self.focused_pane_now = None;
            self.pending_pane_focus = self.workspaces[ws_idx].root.as_ref().and_then(|root| {
                let leaves = root.collect_leaves();
                closed_leaf_idx
                    .and_then(|idx| leaves.get(idx).cloned())
                    .or_else(|| leaves.last().cloned())
            });
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Copy a surface's reference - the globally-disambiguated name the MCP
    /// `list_panes` tool reports - so it can be pasted into an agent's prompt
    /// ("read the logs in cargo-run").
    ///
    /// Falls back to the raw id if the surface vanished between the click and
    /// here.
    /// Put this surface's last answer on the clipboard, as the markdown the
    /// agent wrote.
    ///
    /// Surface-level, not per-message, and that is a property of the app rather
    /// than a shortcut: an agent pane is the CLI's own terminal, so there are no
    /// message blocks to hang a button on. "The last one" is the case the work
    /// actually needs - a coordination session asking an execution session what
    /// it concluded - and it is the only one a surface-level entry can name
    /// without a picker.
    ///
    /// The read is a bounded tail scan of a file an agent writes, so it goes to
    /// a background thread. Nothing on the clipboard changes until it comes
    /// back: a failed read must leave whatever the user had there alone rather
    /// than replacing it with an error string.
    pub(crate) fn copy_last_answer(
        &mut self,
        session_id: String,
        cwd: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        // Defense in depth, and the only place the id crosses into a
        // *filename*. Every writer of `Thread::session_id` gates on this
        // already - the mint path, the restore path, `ai.session_start`, and
        // the state pass adopting the session a `/clear` moved the agent onto
        // (`claude_pid_state::session_from_pid_file`) - so this cannot fire
        // today; it is here because those four guarantees live in four other
        // files, and the allow-list happening to exclude `/` and `.` is what
        // keeps this `join` inside the project directory.
        if !crate::agent_sessions::is_valid_session_id(&session_id) {
            log::warn!("last answer: bound session id failed the allow-list, refusing to read");
            return;
        }
        let cwd_str = cwd.to_string_lossy().into_owned();
        cx.spawn(async move |this, cx| {
            let answer = smol::unblock(move || {
                crate::claude_sessions::read_last_answer(&cwd_str, &session_id)
            })
            .await;
            let _ = this.update(cx, |app, cx| match answer {
                crate::claude_sessions::LastAnswer::Found(answer) => {
                    let chars = answer.chars().count();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(answer));
                    app.show_toast(format!("Copied the last answer ({chars} characters)"), cx);
                }
                // Three different nothings, three messages. They used to be
                // one, on the theory that the user could act on none of them,
                // and that is what made the one that was our fault look exactly
                // like a session with nothing to say. "Not yet" wants waiting
                // for; "no transcript" is a session that has not been sent
                // anything, or one the lookup cannot see; "could not read" is
                // the machine. The path stays in the log rather than the toast.
                crate::claude_sessions::LastAnswer::NotYet => {
                    app.show_toast("No answer to copy yet", cx);
                }
                crate::claude_sessions::LastAnswer::NoTranscript => {
                    app.show_toast("No transcript for this session on disk yet", cx);
                }
                crate::claude_sessions::LastAnswer::Unreadable => {
                    app.show_toast("Could not read this session's transcript", cx);
                }
            });
        })
        .detach();
    }

    /// The same copy, for a surface named by its rail row or its slot header.
    ///
    /// It used to go through the rendered transcript's own cache, which meant
    /// the entry could only work for a face that was on screen and refused with
    /// "switch the agent surface to it first" otherwise. The reader it needs is
    /// the one the session-history menu was already using: the CLI's own
    /// `.jsonl`, which is there whether anything is drawing it or not.
    ///
    /// The gate is [`crate::claude_sessions::transcript_path`] and not a pair of
    /// checks written out here, because that function is the one home for "whose
    /// file can this build read" - the same one the state detector, the model
    /// probe and this menu's own `can_copy` ask. Asking it rather than the thread
    /// is also what keeps the chord and the menu entry agreeing: the menu hides
    /// itself for a surface with no readable transcript, and without this the
    /// chord would go on to read a Claude-shaped path for, say, a Codex session
    /// and report "no answer yet" - a claim about that session rather than about
    /// the limits of this build.
    pub(crate) fn copy_last_answer_for_agents_target(
        &mut self,
        target: crate::project::AgentsTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.thread_for_target(target) else {
            return;
        };
        if crate::claude_sessions::transcript_path(thread).is_none() {
            self.show_toast(
                "This session has not written a transcript Splitlane can read",
                cx,
            );
            return;
        }
        let (Some(session_id), cwd) = (
            thread.session_id.clone(),
            std::path::PathBuf::from(&thread.cwd),
        ) else {
            return;
        };
        self.copy_last_answer(session_id, cwd, cx);
    }

    /// The design's Alt-Cmd-C, aimed at whichever agent surface is focused.
    pub(crate) fn handle_copy_last_answer(
        &mut self,
        _action: &crate::CopyLastAnswer,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.current_thread_view_target(cx) else {
            return;
        };
        self.copy_last_answer_for_agents_target(target, cx);
    }

    pub(crate) fn copy_surface_ref(&mut self, surface_id: u64, cx: &mut Context<Self>) {
        let reference = self
            .collect_surface_meta(cx)
            .into_iter()
            .find(|m| m.surface_id == surface_id)
            .map(|m| m.name)
            .unwrap_or_else(|| surface_id.to_string());
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(reference.clone()));
        self.show_toast(format!("Copied surface ref: {reference}"), cx);
    }

    pub(crate) fn handle_pane_event(
        &mut self,
        pane: Entity<Pane>,
        event: &pane::PaneEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            pane::PaneEvent::Remove => {
                self.drop_pane_from_layout(&pane, cx);
            }
            pane::PaneEvent::LauncherPick(action) => {
                // The pane named what was picked; filling it is the app's job,
                // because a pane knows a `workspace_id` and not a directory.
                self.launcher_pick(&pane, action.clone(), cx);
            }
            pane::PaneEvent::DropRailSurface { ws_idx, thread_id } => {
                // Resolved from the id, not from the index the drag started
                // with: the rail can be reordered while the pointer is down.
                let Some(thread_idx) = self.thread_index_by_id(*ws_idx, *thread_id) else {
                    return;
                };
                self.dragging_rail_surface = None;
                self.pending_rail_surface_drop = Some((*ws_idx, thread_idx, pane.clone()));
                cx.notify();
            }
            pane::PaneEvent::CloseSlot => {
                // The header's `\u{d7}`. The same removal, plus the undo record -
                // a slot the user closed on purpose is exactly the one they
                // may want back (`undo_close_pane`).
                //
                // An agent surface in this slot is parked, not lost: its
                // placement is derived from the tree, so dropping the leaf is
                // all that "park it again" means.
                let Some(ws_idx) = self
                    .workspaces
                    .iter()
                    .position(|ws| ws.contains_pane(&pane))
                else {
                    return;
                };
                self.record_closed_pane(&pane, ws_idx, cx);
                self.drop_pane_from_layout(&pane, cx);
            }
            pane::PaneEvent::SurfaceRenamed {
                thread_id,
                name,
                display,
            } => {
                // A tab's custom name changed - persist so it survives
                // restart (the name rides in the layout's SurfaceDefinition).
                //
                // And the rail row is the same surface: it takes its label
                // from the record, so a rename that wrote only the terminal's
                // `custom_name` showed in the header and nowhere else. Write
                // both, and mark the record user-set so the OSC title and the
                // ai-title backfill leave the deliberate name alone - exactly
                // what a rename in the rail already does.
                if let Some(thread_id) = *thread_id
                    && let Some(thread) = self.thread_by_id_mut(thread_id)
                {
                    // Both halves, both ways. Clearing the name used to lift
                    // the lock and leave `title` standing, so the header fell
                    // back to the derived name while the rail went on drawing
                    // the one that had just been erased - the same divergence
                    // this arm exists to close, on the undo edge.
                    thread.title = name.clone().unwrap_or_else(|| display.clone());
                    thread.title_user_set = name.is_some();
                }
                self.save_session(cx);
                cx.notify();
            }
            pane::PaneEvent::TabsChanged => {
                self.save_session(cx);
                cx.notify();
            }
            pane::PaneEvent::OpenTabMenu { tab_id, position } => {
                // Open the "Move to pane…" menu for this tab.
                // Mutually exclusive with the other popovers, matching the
                // workspace/profile/sessions menu pattern.
                self.dismiss_transient_surfaces();
                self.tab_menu_open = Some(crate::TabContextMenu {
                    source_pane: pane.clone(),
                    tab_id: *tab_id,
                    position: *position,
                });
                cx.notify();
            }
            pane::PaneEvent::DropSessionSplit {
                edge,
                agent,
                session_id,
                cwd,
            } => {
                // A session row was dropped out of the sidebar onto a pane.
                // Spawn a fresh terminal at the session's cwd running the
                // agent's resume command, then split the target pane toward the
                // previewed edge (or append it here as a tab for center).
                let edge = *edge;
                let agent = *agent;
                let session_id = session_id.clone();
                let cwd = cwd.clone();
                let target = pane.clone(); // the emitting pane is the target

                let Some(ws_idx) = self
                    .workspaces
                    .iter()
                    .position(|ws| ws.contains_pane(&target))
                else {
                    return;
                };

                // A split adds one pane - refuse at the cap (edge case #5). A
                // center drop appends a tab to an existing pane, so it doesn't
                // grow the count and isn't capped.
                if edge.is_some()
                    && self.workspaces[ws_idx]
                        .root
                        .as_ref()
                        .map(|r| r.leaf_count())
                        .unwrap_or(0)
                        >= MAX_PANES
                {
                    return;
                }

                let ws_id = self.workspaces[ws_idx].id;
                let cwd_path = (!cwd.is_empty()).then(|| std::path::PathBuf::from(&cwd));
                let term = cx.new(|cx| {
                    TerminalView::with_cwd_and_profile(
                        ws_id,
                        cwd_path,
                        None,
                        TerminalSurfaceProfile::Agent,
                        cx,
                    )
                });
                // Resume the picked session in the new terminal. Honors the
                // Claude bypass flag exactly like a tab-bar launch. Skips the
                // send if the id fails the allow-list (defence-in-depth).
                if let Some(resume) = crate::app::sessions_sidebar::resume_command(
                    agent,
                    &session_id,
                    &self.cached_config,
                ) {
                    term.read(cx).send_command(&resume);
                }

                match edge {
                    Some(edge) => {
                        // `create_pane` wires the app-level CWD/port subscription
                        // and the pane-event subscription (mirrors `DropSplit`).
                        let new_pane = self.create_pane(term, cx);
                        let inserted = if let Some(root) = &mut self.workspaces[ws_idx].root {
                            split_pane_at_edge(root, &target, edge, new_pane.clone())
                        } else {
                            false
                        };
                        if !inserted {
                            return;
                        }
                        self.pending_pane_focus = Some(new_pane);
                    }
                    None => {
                        // Center drop: the resumed session becomes what this
                        // pane shows. It used to be *appended* as a second tab,
                        // which was survivable while a tab strip could draw it
                        // and is not now: a pane holds one surface, so the
                        // second was a running PTY nothing could show, select
                        // or close. What it displaces is parked first, exactly
                        // as it is for the rail's own "Show in <pane>".
                        cx.subscribe(&term, Self::handle_terminal_event).detach();
                        if let Some(displaced) = target.read(cx).surface() {
                            self.park_displaced_surface(ws_idx, &displaced, cx);
                        }
                        target.update(cx, |p, cx| {
                            p.show_terminal(term, cx);
                        });
                        self.pending_pane_focus = Some(target);
                    }
                }
                self.save_session(cx);
                cx.notify();
            }
            pane::PaneEvent::DropMarkdownSplit { edge, path } => {
                // A markdown row was dropped out of the Files sidebar onto a
                // pane. Open it via the existing `FileView`
                // API, then split the target toward the previewed edge or append
                // it here as a tab (center). Mirrors `DropSessionSplit`, minus
                // the terminal spawn.
                let edge = *edge;
                let path = path.clone();
                let target = pane.clone(); // the emitting pane is the target

                let Some(ws_idx) = self
                    .workspaces
                    .iter()
                    .position(|ws| ws.contains_pane(&target))
                else {
                    return;
                };

                // A split adds one pane - refuse at the cap (edge case #9). A
                // center drop appends a tab, so it isn't capped.
                if edge.is_some()
                    && self.workspaces[ws_idx]
                        .root
                        .as_ref()
                        .map(|r| r.leaf_count())
                        .unwrap_or(0)
                        >= MAX_PANES
                {
                    return;
                }

                let markdown = cx.new(|cx| crate::file_view::FileView::open(path, cx));

                match edge {
                    Some(edge) => {
                        let new_pane = self.create_pane_with_existing_tab(
                            crate::pane::TabContent::Markdown(markdown),
                            cx,
                        );
                        let inserted = if let Some(root) = &mut self.workspaces[ws_idx].root {
                            split_pane_at_edge(root, &target, edge, new_pane.clone())
                        } else {
                            false
                        };
                        if !inserted {
                            return;
                        }
                        self.pending_pane_focus = Some(new_pane);
                    }
                    None => {
                        // Center drop: append the markdown as a new tab in the
                        // target pane (mirrors the click-to-open path).
                        target.update(cx, |p, cx| {
                            p.show_markdown(markdown, cx);
                        });
                        self.pending_pane_focus = Some(target);
                    }
                }
                self.save_session(cx);
                cx.notify();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Terminal event handling - push-based port detection and CWD tracking
    // -----------------------------------------------------------------------

    pub(crate) fn handle_terminal_event(
        &mut self,
        terminal: Entity<TerminalView>,
        event: &terminal::TerminalEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            terminal::TerminalEvent::ActivityBurst => {
                if let Some(ws_idx) = self.workspace_idx_for_terminal(&terminal, cx) {
                    self.schedule_port_scan(ws_idx, cx);
                }
            }
            terminal::TerminalEvent::CwdChanged(new_cwd) => {
                self.handle_cwd_change(&terminal, new_cwd, cx);
            }
            terminal::TerminalEvent::ServiceDetected(info) => {
                // Remember which terminal announced this port
                // so the next scan can cross-check the announcement against
                // the actual LISTEN owner (collision badge).
                terminal.update(cx, |view, _| view.terminal.note_announced_port(info.port));
                if let Some(ws_idx) = self.workspace_idx_for_terminal(&terminal, cx) {
                    let ws = &mut self.workspaces[ws_idx];
                    let mut terminal_info = info.clone();
                    terminal_info.is_frontend = false;
                    if merge_service_label(&mut ws.service_labels, terminal_info)
                        && self.settings_section.is_none()
                    {
                        cx.notify();
                    }
                }
            }
            terminal::TerminalEvent::CancelSwapMode => {
                self.cancel_swap_mode(cx);
            }
            terminal::TerminalEvent::SelectionCopied => {
                self.show_toast("Copied", cx);
            }
            terminal::TerminalEvent::OpenMarkdownPath(path) => {
                self.open_markdown_in_pane(&terminal, path.clone(), cx);
            }
            terminal::TerminalEvent::FontZoomChanged => {
                // Persist immediately so the zoom survives a
                // crash, not just a clean quit (SurfaceRenamed parity).
                self.save_session(cx);
            }
            terminal::TerminalEvent::FleetSearchRequested { query, .. } => {
                // Fleet search used to fan the query out to an overlay of its own.
                // The design dissolves that into the palette's Output tab, so
                // the find bar's "Fleet" control opens the palette there with
                // this query already in the field. The regex flag is dropped:
                // the palette has one query box, and it also filters names and
                // projects, so a stray `(` must not turn it into an error.
                //
                // Deferred through `render`, which is where the `Window` that
                // focusing the field needs comes from.
                self.pending_palette_output = Some(query.clone());
                cx.notify();
            }
            terminal::TerminalEvent::OpenCodePath { path, line, col } => {
                // Spawn the editor on the GPUI background executor so a
                // slow editor launch (cold VS Code, remote SSH editor)
                // never blocks the main thread. `open_at_location`
                // already log-swallows failures, so we don't need to
                // surface the result here.
                let path = path.clone();
                let line = *line;
                let col = *col;
                // Settings -> General's "Default editor" is read here, which is
                // the second of the two doors onto one file. The other is the
                // file surface's `Open in editor`; both go through
                // `open_at_location_with` so a person cannot be sent to two
                // different editors by two gestures that say the same thing.
                let preference = crate::editor::EditorPreference::from_config(&self.cached_config);
                cx.background_executor()
                    .spawn(async move {
                        crate::editor::open_at_location_with(&path, line, col, &preference);
                    })
                    .detach();
            }
            terminal::TerminalEvent::ChildExited => {
                // The Pane's own subscription closes the tab; here we drop
                // the dying surface's agent sessions NOW instead of waiting
                // ≤30s for the sweep. Covers the paths where the shim's
                // `ai.exit`/`ai.session_end` never arrive (shim SIGKILLed,
                // agent launched without the shim).
                self.purge_sessions_for_surface(terminal.entity_id().as_u64(), cx);
            }
            // TitleChanged is handled by Pane's subscription
            _ => {}
        }
    }

    /// Append a markdown tab to the pane that owns `source_terminal`.
    ///
    /// The historical implementation split the layout vertically and created
    /// a dedicated markdown pane; the user feedback was that opening a doc
    /// shouldn't shrink the terminal real-estate. The current behaviour is to
    /// make markdown a peer tab inside the same pane - the user keeps the
    /// terminal+markdown pair via Ctrl+Tab / mouse-click, and the layout tree
    /// is untouched.
    fn open_markdown_in_pane(
        &mut self,
        source_terminal: &Entity<TerminalView>,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        // Same dead end as the Files sidebar, reached from a `.md` hyperlink:
        // an Agents thread is a terminal too, so this fires there - and its
        // workspace lookup would just return `None`, silently.
        if self.refuse_markdown_without_a_pane(cx) {
            return;
        }
        let Some(ws_idx) = self.workspace_idx_for_terminal(source_terminal, cx) else {
            return;
        };
        let source_pane = self.workspaces[ws_idx].root.as_ref().and_then(|root| {
            root.collect_leaves()
                .into_iter()
                .find(|pane| pane.read(cx).contains_terminal(source_terminal))
        });
        let Some(source_pane) = source_pane else {
            return;
        };

        let path_for_pane = path.clone();
        let markdown = cx.new(|cx: &mut Context<crate::file_view::FileView>| {
            crate::file_view::FileView::open(path_for_pane, cx)
        });

        source_pane.update(cx, |pane, cx| {
            pane.show_markdown(markdown, cx);
            cx.notify();
        });
        self.save_session(cx);
        cx.notify();
    }

    /// Find which workspace contains the given terminal entity.
    fn workspace_idx_for_terminal(
        &self,
        terminal: &Entity<TerminalView>,
        cx: &App,
    ) -> Option<usize> {
        self.workspaces
            .iter()
            .position(|ws| ws.any_pane(|pane| pane.read(cx).contains_terminal(terminal)))
    }

    /// Immediately drop agent sessions anchored to a dying surface (the
    /// shell behind it exited - its tab is closing), plus any real-PID
    /// session of the same pass whose process is already gone. Surgical
    /// complement to [`Self::sweep_stale_pids`]: same retention semantics,
    /// zero latency instead of ≤30s, no Stalled logic. An `Errored` session
    /// on the dying surface is dropped too - that matches the sweep's
    /// "sticky until its pane closes" contract, just without the wait.
    pub(crate) fn purge_sessions_for_surface(&mut self, surface_id: u64, cx: &mut Context<Self>) {
        let mut changed = false;
        for ws in &mut self.workspaces {
            if ws.agent_sessions.is_empty() {
                continue;
            }
            let before = ws.agent_sessions.len();
            ws.agent_sessions.retain(|&pid, session| {
                // Opportunistic: a session never resolved to a surface can
                // only be reaped through its PID - probe it now (the dying
                // shell may have taken the agent with it via SIGHUP).
                keep_session_after_surface_purge(surface_id, pid, session)
            });
            if ws.agent_sessions.len() < before {
                changed = true;
            }
        }
        if changed {
            // Same post-mutation trio as the sweep: drop orphan pane glows,
            // flush queued prompts stranded on the dead session, repaint.
            self.sync_attention(cx);
            self.agent_sessions_changed(cx);
            cx.notify();
        }
    }

    /// Probe registered AI agent PIDs and clean up stale entries where the
    /// process no longer exists. See [`pid_is_alive`] for the per-platform
    /// probe (Unix: `kill(pid, 0)` / `ESRCH`; Windows: `OpenProcess` null
    /// handle; other: conservative keep).
    pub(crate) fn sweep_stale_pids(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        // Surfaces that still resolve to a live terminal tab.
        // An `Errored` session's PID is dead by definition (the binary
        // exited) - it is spared from the PID reap WHILE its pane lives so
        // the crash signal stays visible, and reaped here once the pane
        // closes. An Errored session that never resolved a surface has no
        // visible anchor beyond the sidebar; it follows the plain PID reap
        // (≤ 30 s) so unresolvable rows can never accumulate.
        let live_surfaces: std::collections::HashSet<u64> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.collect_panes())
            .flat_map(|pane| {
                pane.read(cx)
                    .terminals()
                    .map(|t| t.entity_id().as_u64())
                    .collect::<Vec<_>>()
            })
            .collect();
        // Stalled detection (default ON, threshold default 60 s, both
        // hot-reload aware via `cached_config`). The sweep runs every 30 s, so
        // the effective detection latency is threshold + up to 30 s granularity
        // (documented in the JSON-schema description).
        let stall_enabled = self.cached_config.agent_stall_detection_enabled();
        let stall_threshold = std::time::Duration::from_secs(
            self.cached_config.resolved_agent_stall_threshold_secs(),
        );
        // The surface id rides along so the notification can ask whether the
        // person can see the pane it is about - the gate is visibility, not
        // window focus.
        let mut stalled_notifs: Vec<(
            crate::agent_launcher::TerminalAgent,
            String,
            u64,
            Option<u64>,
        )> = Vec::new();
        for ws in &mut self.workspaces {
            if ws.agent_sessions.is_empty() {
                continue;
            }
            let before = ws.agent_sessions.len();
            // Synthetic PIDs (from the upsert fallback for legacy shims
            // without `pid` on every frame) are stored in the high half
            // of u32 - outside the OS-assignable range on all supported
            // platforms - so probing them with `kill(pid, 0)` would
            // always say "dead" and immediately drop a live legacy
            // session. Keep them around: they'll be cleared by
            // `ai.session_end` or by the next state transition.
            ws.agent_sessions.retain(|&pid, session| {
                stale_sweep_keeps_without_pid_probe(pid, session, &live_surfaces)
                    || pid_matches(pid, session.proc_start)
            });
            if ws.agent_sessions.len() < before {
                changed = true;
            }
            // A `Thinking` session silent past the threshold flips
            // to `Stalled`. Only `Thinking` flips, so the once-per-episode
            // notification dedup is structural: the session stays Stalled
            // (this branch can't re-trigger) until a hook event revives it,
            // and a NEW episode requires a fresh Thinking phase first.
            if stall_enabled {
                for session in ws.agent_sessions.values_mut() {
                    if session
                        .state
                        .stalls_after(session.last_activity.elapsed(), stall_threshold)
                    {
                        session.state = ai_types::AgentState::Stalled;
                        // This write bypasses `upsert_session_state`, so hold
                        // its invariant by hand: only WaitingForInput carries
                        // a wait stamp. A Thinking row is already None, but
                        // clear defensively rather than rely on that.
                        session.waiting_since = None;
                        stalled_notifs.push((
                            session.tool,
                            ws.title.clone(),
                            session.last_activity.elapsed().as_secs(),
                            session.surface_id,
                        ));
                        changed = true;
                    }
                }
            }
        }
        // Agents-view threads: a CLI killed mid-turn never sends `ai.stop`,
        // which would leave the row spinner running forever. Same
        // conservative policy as above - a thread whose hook frames carried
        // no PID is kept as-is (cleared by `ai.stop` / `ai.session_end`).
        for t in self
            .workspaces
            .iter_mut()
            .flat_map(|p| p.threads.iter_mut())
        {
            if t.status != crate::project::ThreadStatus::Idle
                && let Some(pid) = t.agent_pid
                && !pid_matches(pid, t.agent_proc_start)
            {
                t.status = crate::project::ThreadStatus::Idle;
                t.agent_pid = None;
                t.agent_proc_start = None;
                changed = true;
            }
        }
        if changed {
            // A swept session may have been
            // driving a pane glow - resync so no orphan attention survives.
            self.sync_attention(cx);
            // A swept `Thinking` session leaves
            // a bare shell - flush (or drop) its queued prompt now, else the
            // buffer and the "1 queued" chip strand forever (no further
            // `ai.*` frame will ever arrive for the dead session).
            self.agent_sessions_changed(cx);
            cx.notify();
        }
        // Fire AFTER the state writes so the notification and
        // the UI agree. One entry per Thinking→Stalled transition == one
        // notification per stall episode (the dedup contract).
        for (agent, title, silent_secs, surface_id) in stalled_notifs {
            let seen = self.surface_is_seen(surface_id, cx);
            super::ipc_handler::fire_stalled_notification(
                agent,
                &title,
                silent_secs,
                &self.cached_config,
                seen,
                cx.background_executor().clone(),
            );
        }
    }

    /// Schedule a debounced port-scan ladder for the given workspace.
    ///
    /// `port_scan_pending` absorbs bursts while a ladder is in flight: the
    /// old design bumped the generation on EVERY burst, so sustained output
    /// (an agent streaming for a minute) superseded the 500ms-debounced scan
    /// over and over and no scan ran until the terminal went quiet. The
    /// generation counter stays as the cancellation belt for workspace
    /// close/reuse.
    fn schedule_port_scan(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        let ws = &mut self.workspaces[ws_idx];
        if ws.port_scan_pending {
            return;
        }
        ws.port_scan_pending = true;
        ws.port_scan_generation += 1;
        let generation = ws.port_scan_generation;
        let ws_id = ws.id;

        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                // Debounce: wait 500ms for activity to settle
                smol::Timer::after(std::time::Duration::from_millis(500)).await;

                // Burst scan at 0s, +2s, +6s after debounce
                for delay_ms in [0u64, 2000, 6000] {
                    if delay_ms > 0 {
                        smol::Timer::after(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    let should_continue = cx.update(|cx| {
                        this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                            app.run_port_scan(ws_id, generation, cx)
                        })
                    });
                    match should_continue {
                        Ok(true) => {}
                        _ => break,
                    }
                }

                // Re-arm regardless of how the ladder ended - the next
                // ActivityBurst starts a fresh one.
                let _ = cx.update(|cx| {
                    this.update(cx, |app: &mut Self, _cx| {
                        if let Some(ws) = app.workspaces.iter_mut().find(|ws| ws.id == ws_id) {
                            ws.port_scan_pending = false;
                        }
                    })
                });
            },
        )
        .detach();
    }

    pub(crate) fn schedule_active_port_rescans(&mut self, cx: &mut Context<Self>) {
        let workspace_ids: Vec<u64> = self
            .workspaces
            .iter()
            .filter(|ws| !ws.active_ports.is_empty() && !ws.port_scan_pending)
            .map(|ws| ws.id)
            .collect();

        for ws_id in workspace_ids {
            if let Some(ws_idx) = self.workspaces.iter().position(|ws| ws.id == ws_id) {
                self.schedule_port_scan(ws_idx, cx);
            }
        }
    }

    /// Execute a single per-pane scan for a workspace.
    /// Returns `false` if the scan should be aborted (generation superseded
    /// or workspace removed).
    fn run_port_scan(&mut self, ws_id: u64, generation: u64, cx: &mut Context<Self>) -> bool {
        let ws = match self.workspaces.iter().find(|ws| ws.id == ws_id) {
            Some(ws) if ws.port_scan_generation == generation => ws,
            _ => return false,
        };

        // (terminal entity id, PTY child pid) pairs - the scan partitions
        // the process walk per terminal subtree instead of flattening the
        // workspace into one pid pool.
        let roots: Vec<(u64, u32)> = ws
            .collect_panes()
            .iter()
            .flat_map(|pane| {
                pane.read(cx)
                    .terminals()
                    .filter_map(|tv| {
                        let child_pid = tv.read(cx).terminal.child_pid;
                        (child_pid > 0).then_some((tv.entity_id().as_u64(), child_pid))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        if roots.is_empty() {
            return true;
        }

        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                // One unified subtree walk per tick feeds ports AND agent
                // identity (the pre-refactor code walked the descendants
                // once for each - this is the strictly-cheaper single pass).
                let scan = smol::unblock(move || {
                    let agent_binaries: Vec<&'static str> =
                        crate::agent_launcher::TerminalAgent::ALL
                            .iter()
                            .map(|a| a.binary())
                            .collect();
                    crate::workspace::scan_panes(&roots, &agent_binaries)
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                        app.apply_pane_scan(ws_id, generation, scan, cx);
                    })
                });
            },
        )
        .detach();
        true
    }

    /// Deposit a finished per-pane scan on the main thread.
    ///
    /// Writes the per-terminal truth (identity-pill agent, port
    /// badges + collision flags) onto each LIVE terminal, then
    /// refreshes the workspace aggregates the sidebar reads - fed with the
    /// union of the per-pane results, identically to the pre-refactor flat
    /// scan (zero sidebar regression). A pane closed between
    /// scan and deposit is naturally dropped: the deposit iterates the
    /// live tree, so its scan entry never matches.
    fn apply_pane_scan(
        &mut self,
        ws_id: u64,
        generation: u64,
        scan: std::collections::HashMap<u64, crate::workspace::PaneScan>,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self
            .workspaces
            .iter_mut()
            .find(|ws| ws.id == ws_id && ws.port_scan_generation == generation)
        else {
            return;
        };

        // Workspace aggregates (sidebar contract).
        let mut changed = merge_scan_workspace_state(
            &mut ws.active_ports,
            &mut ws.service_labels,
            &mut ws.detected_agents,
            &scan,
        );

        // Snapshot for the per-terminal announce-dedup purge below (ends the
        // mutable borrow region cleanly before the pane loop).
        let live_ports: Vec<u16> = ws.active_ports.clone();

        // Frontend URLs for live per-terminal service state (sidebar parity:
        // only frontend services get a link, backend ports stay textual).
        let frontend_urls: std::collections::HashMap<u16, String> = ws
            .service_labels
            .iter()
            .filter(|(_, info)| info.is_frontend)
            .filter_map(|(port, info)| info.url.clone().map(|u| (*port, u)))
            .collect();

        let leaves: Vec<gpui::Entity<crate::pane::Pane>> = ws.collect_panes();

        // Collision pre-pass: port → owning terminal. A port
        // LISTENed by ≥ 2 subtrees is excluded - that is SO_REUSEPORT-style
        // sharding (nginx workers, `reusePort` servers), intentional load
        // balancing, not a collision. Other known false positives (proxies,
        // port-forwards, re-announcements after a restart) are tolerated in
        // v1 - the badge is an info-level heuristic, never blocking.
        let (owner, shared) = port_ownership(&scan);

        // Owner display names for the conflict tooltip (custom name, else
        // OSC title, else a stable surface reference). The OSC title is
        // UNTRUSTED terminal-controlled text and this tooltip is a new sink
        // for it: strip bidi/zero-width controls (an RLO could visually
        // reverse the surrounding `port N is owned by "…"` and spoof the
        // owner) and clamp the length (an unbounded title would otherwise
        // inflate the tooltip and this per-tick map). The custom name is
        // user-typed and already bounded, but it rides the same scrub -
        // one path, no exceptions.
        let mut display_names: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        for pane in &leaves {
            for tv in pane.read(cx).terminals() {
                let tid = tv.entity_id().as_u64();
                let r = tv.read(cx);
                let name = r
                    .terminal
                    .custom_name
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        if r.terminal.title.is_empty() {
                            format!("surface {tid}")
                        } else {
                            r.terminal.title.clone()
                        }
                    });
                let name = crate::markdown::strip_bidi_zero_width(name.chars().take(64).collect());
                display_names.insert(tid, name);
            }
        }

        for pane in &leaves {
            let terminals: Vec<gpui::Entity<crate::terminal::TerminalView>> =
                pane.read(cx).terminals().cloned().collect();
            let mut pane_changed = false;
            for tv in terminals {
                let tid = tv.entity_id().as_u64();
                // A terminal spawned after the scan's root collection has no
                // entry - leave it untouched (the burst's next tick or the
                // next activity scan covers it).
                let Some(s) = scan.get(&tid) else {
                    continue;
                };
                let agent = s
                    .agents
                    .first()
                    .and_then(|b| crate::agent_launcher::TerminalAgent::from_binary(b));
                tv.update(cx, |view, _cx| {
                    let t = &mut view.terminal;
                    // A port that left LISTEN must become re-announceable -
                    // a dev server restarted inside a live shell (nodemon,
                    // plain re-run) re-prints its banner, and that line must
                    // re-fire ServiceDetected (the dedup was previously
                    // cleared only on ChildExit).
                    t.retain_reported_ports(&live_ports);
                    if t.detected_agent != agent || !t.agent_confirmed {
                        // The live scan owns the value from here on - this
                        // both confirms a restored "last known" pill and
                        // clears a stale one.
                        t.detected_agent = agent;
                        t.agent_confirmed = true;
                        pane_changed = true;
                    }
                    let ports_with_links: Vec<(u16, Option<String>)> = s
                        .ports
                        .iter()
                        .map(|e| (e.port, frontend_urls.get(&e.port).cloned()))
                        .collect();
                    if t.detected_ports != ports_with_links {
                        t.detected_ports = ports_with_links;
                        pane_changed = true;
                    }
                    if t.cached_foreground_command != s.foreground_command {
                        t.cached_foreground_command = s.foreground_command.clone();
                        pane_changed = true;
                    }
                    let conflicts = announced_port_conflicts(
                        &t.announced_ports,
                        tid,
                        &owner,
                        &shared,
                        &display_names,
                    );
                    if t.port_conflicts != conflicts {
                        t.port_conflicts = conflicts;
                        pane_changed = true;
                    }
                });
            }
            if pane_changed {
                // The tab strip renders from the terminals' state - nudge
                // the pane so the pill/badges repaint on this frame.
                pane.update(cx, |_, cx| cx.notify());
                changed = true;
            }
        }

        if changed {
            cx.notify();
        }
    }

    /// Handle a CWD change from a terminal. Only processes if the terminal is
    /// the active tab of a pane in its workspace (background terminals ignored).
    fn handle_cwd_change(
        &mut self,
        terminal: &Entity<TerminalView>,
        new_cwd: &str,
        cx: &mut Context<Self>,
    ) {
        // Find workspace where this terminal is the active tab in any pane.
        // Skip markdown panes - they have no active terminal, so the
        // identity check via `active_terminal_opt` returns None for them.
        let ws_idx = self.workspaces.iter().position(|ws| {
            ws.root.as_ref().is_some_and(|root| {
                root.any_leaf(&mut |pane| {
                    pane.read(cx)
                        .active_terminal_opt()
                        .is_some_and(|t| *t == *terminal)
                })
            })
        });
        let Some(ws_idx) = ws_idx else { return };

        if self.workspaces[ws_idx].cwd == new_cwd {
            return;
        }

        // Capture the stable workspace id, NOT the positional index.
        // The git probe below awaits (long on big repos / network FS); during
        // that await the main loop can run close/reorder/IPC-close and compact
        // the `Vec`, so a reused `ws_idx` would point at a *different*
        // workspace (silent git-state corruption + watch refcount desync).
        // Re-resolve the index by identity after the await - model:
        // `run_port_scan` / `spawn_initial_git_stats`.
        let ws_id = self.workspaces[ws_idx].id;

        let new_cwd_owned = new_cwd.to_string();

        // Run git probe off main thread
        cx.spawn({
            let new_cwd = new_cwd_owned.clone();
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let (git_dir, branch, is_repo, stats) = smol::unblock({
                    let cwd = new_cwd.clone();
                    move || {
                        let git_dir = crate::workspace::find_git_dir(&cwd);
                        let (branch, is_repo) = crate::workspace::detect_branch(&cwd);
                        let stats = crate::workspace::GitDiffStats::from_cwd(&cwd);
                        (git_dir, branch, is_repo, stats)
                    }
                })
                .await;

                let _ = cx.update(|cx| {
                    this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                        // Re-resolve by identity: the workspace may have been
                        // closed or reordered during the await.
                        let Some(ws_idx) = app.workspaces.iter().position(|ws| ws.id == ws_id)
                        else {
                            return;
                        };
                        // Unwatch old git dir
                        let old_git_dir = app.workspaces[ws_idx].git_dir.clone();
                        if let Some(ref dir) = old_git_dir {
                            app.unwatch_git_dir(dir);
                        }
                        // Update workspace git tracking (cwd stays fixed at creation -
                        // it represents the workspace's root folder and must not drift
                        // when the user `cd`s inside the shell).
                        let tracked_cwd = {
                            let ws = &mut app.workspaces[ws_idx];
                            ws.git_dir = git_dir.clone();
                            ws.cwd.clone()
                        };
                        // Watch new git dir
                        if let Some(ref dir) = git_dir {
                            let count = app.git_watch_counts.entry(dir.clone()).or_insert(0);
                            *count += 1;
                            if *count == 1
                                && let Some(ref mut watcher) = app.git_watcher
                                && let Err(e) =
                                    watcher.watch(dir, notify::RecursiveMode::NonRecursive)
                            {
                                log::warn!("git watcher: failed to watch {}: {e}", dir.display());
                            }
                        }
                        let changed =
                            app.apply_git_state_for_cwd(&tracked_cwd, branch, is_repo, stats);
                        log::debug!("workspace CWD changed to: {new_cwd}");
                        if changed {
                            cx.notify();
                        }
                    })
                });
            }
        })
        .detach();
    }

    /// Populate a freshly-created workspace's `git diff --shortstat`
    /// stats off the GPUI main thread. The constructors build with
    /// `git_stats: default()` (0/0) so the blocking `git` subprocess never runs
    /// on the render thread; this spawns it via `smol::unblock` and re-injects
    /// the result, keyed by the stable `ws_id` (another workspace may be
    /// created/closed during the await - ids are stable, indices are not). Mirrors
    /// [`handle_cwd_change`].
    pub(crate) fn spawn_initial_git_stats(ws_id: u64, cwd: String, cx: &mut Context<Self>) {
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let cwd_for_apply = cwd.clone();
                let (branch, is_repo, stats) = smol::unblock(move || {
                    let (branch, is_repo) = crate::workspace::detect_branch(&cwd);
                    let stats = crate::workspace::GitDiffStats::from_cwd(&cwd);
                    (branch, is_repo, stats)
                })
                .await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                        if app.workspaces.iter().any(|ws| ws.id == ws_id)
                            && app.apply_git_state_for_cwd(&cwd_for_apply, branch, is_repo, stats)
                        {
                            cx.notify();
                        }
                    })
                });
            },
        )
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        announced_port_conflicts, keep_session_after_surface_purge, merge_scan_workspace_state,
        merge_service_label, parse_proc_stat_starttime, port_ownership,
        stale_sweep_keeps_without_pid_probe,
    };
    use crate::agent_launcher::TerminalAgent;
    use crate::ai_types::{AgentSession, AgentState};
    use crate::terminal::ServiceInfo;
    use crate::workspace::{PaneScan, PortEntry};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn proc_stat_starttime_survives_hostile_comm_names() {
        // Plain comm: starttime is the 22nd field (9876543 here).
        let plain = "1234 (zsh) S 1 1234 1234 0 -1 4194304 0 0 0 0 5 3 0 0 20 0 11 0 9876543 123 456 18446744073709551615";
        assert_eq!(parse_proc_stat_starttime(plain), Some(9876543));
        // Comm with spaces AND parens - split must anchor on the LAST ')'.
        let hostile = "1234 (next-server (v15)) S 1 1234 1234 0 -1 4194304 0 0 0 0 5 3 0 0 20 0 11 0 424242 123 456";
        assert_eq!(parse_proc_stat_starttime(hostile), Some(424242));
        // Truncated content (fewer than 22 fields) yields None, not a panic.
        assert_eq!(parse_proc_stat_starttime("1234 (zsh) S 1 1234"), None);
        assert_eq!(parse_proc_stat_starttime(""), None);
    }

    #[test]
    fn surface_purge_drops_sessions_bound_to_dying_surface() {
        let mut session = AgentSession::new(TerminalAgent::ClaudeCode, AgentState::Errored);
        session.surface_id = Some(7);

        assert!(!keep_session_after_surface_purge(7, u32::MAX, &session));
        assert!(keep_session_after_surface_purge(8, u32::MAX, &session));
    }

    #[test]
    fn stale_sweep_keeps_synthetic_pid_without_os_probe() {
        let session = AgentSession::new(TerminalAgent::ClaudeCode, AgentState::Thinking);
        let live_surfaces = HashSet::new();

        assert!(stale_sweep_keeps_without_pid_probe(
            u32::MAX,
            &session,
            &live_surfaces
        ));
    }

    #[test]
    fn stale_sweep_keeps_errored_session_while_surface_is_live() {
        let mut session = AgentSession::new(TerminalAgent::Codex, AgentState::Errored);
        session.surface_id = Some(42);
        let live_surfaces = HashSet::from([42]);

        assert!(stale_sweep_keeps_without_pid_probe(
            1234,
            &session,
            &live_surfaces
        ));

        let live_surfaces = HashSet::new();
        assert!(!stale_sweep_keeps_without_pid_probe(
            1234,
            &session,
            &live_surfaces
        ));
    }

    #[test]
    fn merge_service_label_keeps_frontend_when_backend_mentions_same_port() {
        let mut labels = HashMap::new();
        assert!(merge_service_label(
            &mut labels,
            ServiceInfo {
                port: 3000,
                url: Some("http://localhost:3000/app".to_string()),
                label: Some("Next.js".to_string()),
                is_frontend: true,
            },
        ));

        assert!(!merge_service_label(
            &mut labels,
            ServiceInfo {
                port: 3000,
                url: Some("http://localhost:3000".to_string()),
                label: Some("Fastify".to_string()),
                is_frontend: false,
            },
        ));

        let info = labels.get(&3000).unwrap();
        assert_eq!(info.label.as_deref(), Some("Next.js"));
        assert_eq!(info.url.as_deref(), Some("http://localhost:3000/app"));
        assert!(info.is_frontend);
    }

    #[test]
    fn merge_scan_workspace_state_adds_frontend_fallback_and_prunes_stale_labels() {
        let mut active_ports = vec![9999];
        let mut service_labels = HashMap::from([(
            9999,
            ServiceInfo {
                port: 9999,
                url: Some("http://localhost:9999".to_string()),
                label: Some("Vite".to_string()),
                is_frontend: true,
            },
        )]);
        let mut detected_agents = HashSet::new();
        let scan = HashMap::from([(
            7,
            PaneScan {
                ports: vec![PortEntry {
                    port: 5173,
                    frontend: Some("Vite"),
                }],
                agents: vec!["codex".to_string()],
                foreground_command: None,
            },
        )]);

        assert!(merge_scan_workspace_state(
            &mut active_ports,
            &mut service_labels,
            &mut detected_agents,
            &scan,
        ));

        assert_eq!(active_ports, vec![5173]);
        assert!(!service_labels.contains_key(&9999));
        let info = service_labels.get(&5173).unwrap();
        assert_eq!(info.url.as_deref(), Some("http://localhost:5173"));
        assert_eq!(info.label.as_deref(), Some("Vite"));
        assert!(info.is_frontend);
        assert!(detected_agents.contains("codex"));
    }

    #[test]
    fn merge_scan_workspace_state_preserves_exact_frontend_url() {
        let mut active_ports = vec![5173];
        let mut service_labels = HashMap::from([(
            5173,
            ServiceInfo {
                port: 5173,
                url: Some("http://localhost:5173/app".to_string()),
                label: Some("Vite".to_string()),
                is_frontend: true,
            },
        )]);
        let mut detected_agents = HashSet::new();
        let scan = HashMap::from([(
            7,
            PaneScan {
                ports: vec![PortEntry {
                    port: 5173,
                    frontend: Some("Vite"),
                }],
                agents: Vec::new(),
                foreground_command: None,
            },
        )]);

        assert!(!merge_scan_workspace_state(
            &mut active_ports,
            &mut service_labels,
            &mut detected_agents,
            &scan,
        ));
        assert_eq!(
            service_labels.get(&5173).unwrap().url.as_deref(),
            Some("http://localhost:5173/app")
        );
    }

    #[test]
    fn merge_scan_workspace_state_downgrades_unconfirmed_frontend_label() {
        let mut active_ports = vec![5173];
        let mut service_labels = HashMap::from([(
            5173,
            ServiceInfo {
                port: 5173,
                url: Some("http://localhost:5173/app".to_string()),
                label: Some("Vite".to_string()),
                is_frontend: true,
            },
        )]);
        let mut detected_agents = HashSet::new();
        let scan = HashMap::from([(
            7,
            PaneScan {
                ports: vec![PortEntry {
                    port: 5173,
                    frontend: None,
                }],
                agents: Vec::new(),
                foreground_command: None,
            },
        )]);

        assert!(merge_scan_workspace_state(
            &mut active_ports,
            &mut service_labels,
            &mut detected_agents,
            &scan,
        ));
        let info = service_labels.get(&5173).unwrap();
        assert!(!info.is_frontend);
        assert_eq!(info.label.as_deref(), Some("Vite"));
        assert_eq!(info.url.as_deref(), Some("http://localhost:5173/app"));
    }

    #[test]
    fn merge_scan_workspace_state_upgrades_terminal_label_from_frontend_scan() {
        let mut active_ports = vec![5173];
        let mut service_labels = HashMap::from([(
            5173,
            ServiceInfo {
                port: 5173,
                url: Some("http://localhost:5173/app".to_string()),
                label: Some("Vite".to_string()),
                is_frontend: false,
            },
        )]);
        let mut detected_agents = HashSet::new();
        let scan = HashMap::from([(
            7,
            PaneScan {
                ports: vec![PortEntry {
                    port: 5173,
                    frontend: Some("Vite"),
                }],
                agents: Vec::new(),
                foreground_command: None,
            },
        )]);

        assert!(merge_scan_workspace_state(
            &mut active_ports,
            &mut service_labels,
            &mut detected_agents,
            &scan,
        ));
        let info = service_labels.get(&5173).unwrap();
        assert!(info.is_frontend);
        assert_eq!(info.url.as_deref(), Some("http://localhost:5173/app"));
    }

    #[test]
    fn announced_port_conflicts_ignore_shared_ports() {
        let shared_scan = HashMap::from([
            (
                1,
                PaneScan {
                    ports: vec![PortEntry {
                        port: 3000,
                        frontend: None,
                    }],
                    agents: Vec::new(),
                    foreground_command: None,
                },
            ),
            (
                2,
                PaneScan {
                    ports: vec![PortEntry {
                        port: 3000,
                        frontend: None,
                    }],
                    agents: Vec::new(),
                    foreground_command: None,
                },
            ),
        ]);
        let (owner, shared) = port_ownership(&shared_scan);
        let display_names = HashMap::from([(1, "frontend".to_string())]);

        assert!(announced_port_conflicts(&[3000], 2, &owner, &shared, &display_names).is_empty());

        let single_owner_scan = HashMap::from([(
            1,
            PaneScan {
                ports: vec![PortEntry {
                    port: 5173,
                    frontend: Some("Vite"),
                }],
                agents: Vec::new(),
                foreground_command: None,
            },
        )]);
        let (owner, shared) = port_ownership(&single_owner_scan);
        let display_names = HashMap::from([(1, "vite pane".to_string())]);

        assert_eq!(
            announced_port_conflicts(&[5173], 2, &owner, &shared, &display_names),
            vec![(5173, "vite pane".to_string())]
        );
    }
}
