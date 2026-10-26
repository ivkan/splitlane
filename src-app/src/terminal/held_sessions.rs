//! What the rest of the app may ask about the sessions the session host
//! holds: which ones there are when the window starts, and what becomes of
//! them when it goes.
//!
//! Everything here is safe to call on every platform and in a window that
//! does not use the host. There it finds nothing and does nothing.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A session the host holds, as a window that has just started sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(host_panes),
    allow(
        dead_code,
        reason = "only a build whose panes can run in the session host lists its sessions"
    )
)]
pub(crate) struct HeldSession {
    /// The session's id, as it is written into a saved layout.
    pub(crate) id: String,
    pub(crate) pid: u32,
    pub(crate) cwd: String,
    /// What the session was started with: a shell.
    pub(crate) program: String,
    /// Milliseconds since the Unix epoch.
    pub(crate) created_at_ms: u64,
    /// The id the session's shell was told is its pane's, by the window
    /// that started it.
    pub(crate) exported_surface: Option<u64>,
    /// The workspace or agent session the shell was told it belongs to.
    pub(crate) exported_workspace: Option<u64>,
}

/// Whether this window opens its panes in the session host.
pub(crate) fn enabled() -> bool {
    #[cfg(host_panes)]
    {
        super::host_session::enabled()
    }
    #[cfg(not(host_panes))]
    {
        false
    }
}

/// What the host held when the window started. `None` until the answer is
/// in.
static LISTING: Mutex<Option<Vec<HeldSession>>> = Mutex::new(None);
static LISTED: Condvar = Condvar::new();

fn publish(held: Vec<HeldSession>) {
    *LISTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(held);
    LISTED.notify_all();
}

/// Start asking the host what it holds. Returns at once; the answer is
/// read with [`listing`]. A window that does not use the host has its
/// answer immediately: nothing.
///
/// Asked on a thread because the answer comes over a socket, and asked
/// before the saved layout is restored because restoring is where it is
/// needed: a pane whose session is still running is attached to it, and
/// one whose session is gone gets a new shell.
pub(crate) fn begin_listing() {
    #[cfg(host_panes)]
    if enabled() {
        let asked = std::thread::Builder::new()
            .name("held-sessions".into())
            .spawn(|| {
                let held = super::host_session::process_endpoint()
                    .map(|endpoint| endpoint.held_sessions())
                    .unwrap_or_default();
                publish(held);
            });
        if asked.is_ok() {
            return;
        }
    }
    publish(Vec::new());
}

/// What the host held when the window started, waiting up to `wait` for
/// the answer. A host that has not answered by then is taken to hold
/// nothing: its sessions are not attached to and not ended, and are found
/// again by the next window.
pub(crate) fn listing(wait: Duration) -> Vec<HeldSession> {
    let until = Instant::now() + wait;
    let mut listing = LISTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if let Some(held) = listing.as_ref() {
            return held.clone();
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            log::warn!(
                target: "splitlane::terminal::host",
                "the session host did not say in time what it holds; restoring as if it held nothing"
            );
            return Vec::new();
        }
        listing = LISTED
            .wait_timeout(listing, left)
            .map(|(guard, _)| guard)
            .unwrap_or_else(|poisoned| poisoned.into_inner().0);
    }
}

/// End a session the host holds, knowing only its id. Returns at once.
///
/// For a session whose record is deleted while this window has not shown
/// it: a window attaches to an agent session's host session when the
/// session is first shown, so until then there is no pane whose going
/// would end it. Ending only the record left the agent running in the
/// host with nothing naming it, found again by the next window as a
/// session nobody could tell apart from the others.
///
/// The host is asked what it holds first: that gives the pid to end the
/// processes by if it stops answering half way, and a session it no
/// longer runs is let be.
pub(crate) fn end_by_id(id: &str) {
    #[cfg(host_panes)]
    if enabled()
        && let (Ok(session), Some(endpoint)) = (
            id.parse::<splitlane_host::SessionId>(),
            super::host_session::process_endpoint(),
        )
    {
        let id = id.to_string();
        let _ = std::thread::Builder::new()
            .name("held-session-end".into())
            .spawn(move || {
                if let Some(held) = endpoint
                    .held_sessions()
                    .into_iter()
                    .find(|held| held.id == id)
                {
                    log::info!(
                        target: "splitlane::terminal::host",
                        "session {id}: its record was deleted before a pane showed it; ending it"
                    );
                    endpoint.end_held(session, held.pid);
                }
            });
    }
    #[cfg(not(host_panes))]
    let _ = id;
}

/// The window is going away and what runs in the host stays.
pub(crate) fn leave_running() {
    #[cfg(host_panes)]
    if enabled()
        && let Some(endpoint) = super::host_session::process_endpoint()
    {
        endpoint.leave_sessions_running();
    }
}

/// Whether it has been decided what becomes of the sessions in the host
/// now that the window is going: they were left running, or the host was
/// asked to end them.
pub(crate) fn settled() -> bool {
    #[cfg(host_panes)]
    if enabled() {
        return super::host_session::process_endpoint().is_none_or(|endpoint| endpoint.leaving());
    }
    true
}

/// How long leaving the app waits for the host to take the request to stop
/// everything.
#[cfg(host_panes)]
const STOP_WAIT: Duration = Duration::from_secs(2);

/// The app is being left with its sessions to be stopped: have the host
/// end everything it holds. Waits, briefly, so it is for the way out and
/// nowhere else.
pub(crate) fn stop_all() {
    #[cfg(host_panes)]
    if enabled()
        && let Some(endpoint) = super::host_session::process_endpoint()
    {
        super::host_session::stop_all(&endpoint, STOP_WAIT);
    }
}

/// End a held session that no pane shows. Returns at once.
pub(crate) fn end(held: &HeldSession) {
    #[cfg(host_panes)]
    if let (Ok(session), Some(endpoint)) = (
        held.id.parse::<splitlane_host::SessionId>(),
        super::host_session::process_endpoint(),
    ) {
        let _ = std::thread::Builder::new()
            .name("held-session-end".into())
            .spawn({
                let pid = held.pid;
                move || endpoint.end_held(session, pid)
            });
    }
    #[cfg(not(host_panes))]
    let _ = held;
}
