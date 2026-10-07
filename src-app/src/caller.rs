//! Which pane an IPC call came from, read off the process table.
//!
//! Every pane exports its surface id to the processes it runs, and a request
//! could simply repeat it. That is a statement by the caller about itself, so
//! nothing that decides what a caller may do can rest on it: any process of the
//! same user can set a variable. What a caller cannot choose is whose child it
//! is. The server reads the pid on the other end of the connection, walks that
//! process's ancestors in one reading of the process table, and the pane is the
//! one whose PTY child is among them.
//!
//! The reading is taken on the connection's own thread, because it is blocking
//! and the handlers run on the thread that draws the window. What crosses to
//! that thread is the chain of pids; matching it against the panes is a pure
//! function over what the app already knows.

use std::collections::HashMap;

use crate::process_tree::ProcessSnapshot;

/// The process on the other end of a connection, as it was when it connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Peer {
    pub pid: u32,
    /// The process's start token at connect time. A pid alone is not an
    /// identity: the peer can exit and the number can be handed to another
    /// process before the request is looked at.
    pub start: Option<u64>,
}

impl Peer {
    /// Note who connected. `pid` is what the socket reported.
    pub(crate) fn connected(pid: Option<i64>) -> Option<Self> {
        let pid = u32::try_from(pid?).ok().filter(|pid| *pid != 0)?;
        Some(Self {
            pid,
            start: crate::process_tree::start_token(pid),
        })
    }
}

/// What the process table said about a caller.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum CallerLineage {
    /// The method does not depend on who calls it, so nothing was read.
    #[default]
    NotAsked,
    /// The caller could not be placed: the socket gave no pid, the table could
    /// not be read, or the pid no longer names the process that connected.
    Unknown,
    /// The caller's pid followed by each ancestor's, nearest first.
    Chain(Vec<u32>),
}

/// The methods whose answer depends on which pane called.
///
/// Kept as a list so the process table is read only for these: an agent's hook
/// sends a frame per tool call, and none of those needs it.
pub(crate) fn method_asks_who_calls(method: &str) -> bool {
    matches!(
        method,
        "surface.add_agent" | "surface.send_text" | "surface.send_keystroke"
    )
}

/// Read the caller's ancestors now. Blocking; call it off the render thread.
pub(crate) fn lineage_now(peer: Option<Peer>) -> CallerLineage {
    let Some(peer) = peer else {
        return CallerLineage::Unknown;
    };
    match ProcessSnapshot::capture() {
        Some(snapshot) => lineage_in(&snapshot, peer),
        None => CallerLineage::Unknown,
    }
}

/// [`lineage_now`] against a reading already taken.
fn lineage_in(snapshot: &ProcessSnapshot, peer: Peer) -> CallerLineage {
    // The pid has to be the same process that connected. Without a token from
    // connect time there is nothing to compare, and "probably the same" is not
    // an answer a permission can be built on.
    let Some(start) = peer.start else {
        return CallerLineage::Unknown;
    };
    if snapshot.identity_of(peer.pid) != Some((peer.pid, start)) {
        return CallerLineage::Unknown;
    }
    let chain = snapshot.lineage_of(peer.pid);
    if chain.is_empty() {
        CallerLineage::Unknown
    } else {
        CallerLineage::Chain(chain)
    }
}

/// The pane a caller is running in: the one whose PTY child is the caller or
/// its nearest ancestor that is one.
///
/// `None` is "in no pane of this app", and it is the answer for a process that
/// detached from the tree it was started in. Such a process is reparented to
/// init, so nothing above it names a pane any more; it is refused the rights a
/// pane has rather than given the benefit of the doubt.
pub(crate) fn pane_of<T: Copy>(
    lineage: &CallerLineage,
    pty_children: &HashMap<u32, T>,
) -> Option<T> {
    let CallerLineage::Chain(chain) = lineage else {
        return None;
    };
    chain.iter().find_map(|pid| pty_children.get(pid).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT: u32 = 1;
    const APP: u32 = 50;
    const SHELL_A: u32 = 100;
    const SHELL_B: u32 = 200;

    /// `init -> app -> two pane shells`, with an agent and its tool call under
    /// the first shell and a detached process under init.
    fn table() -> ProcessSnapshot {
        ProcessSnapshot::from_edges(&[
            (APP, INIT),
            (SHELL_A, APP),
            (SHELL_B, APP),
            (110, SHELL_A),
            (111, 110),
            (112, 111),
            (210, SHELL_B),
            (900, INIT),
        ])
    }

    fn panes() -> HashMap<u32, u64> {
        HashMap::from([(SHELL_A, 7), (SHELL_B, 8)])
    }

    fn peer(snapshot: &ProcessSnapshot, pid: u32) -> Peer {
        Peer {
            pid,
            start: snapshot.identity_of(pid).map(|(_, start)| start),
        }
    }

    #[test]
    fn a_tool_call_three_deep_is_placed_in_its_pane() {
        let snapshot = table();
        let lineage = lineage_in(&snapshot, peer(&snapshot, 112));
        assert_eq!(pane_of(&lineage, &panes()), Some(7));
        let lineage = lineage_in(&snapshot, peer(&snapshot, 210));
        assert_eq!(pane_of(&lineage, &panes()), Some(8));
    }

    #[test]
    fn the_pane_shell_itself_is_in_its_pane() {
        let snapshot = table();
        let lineage = lineage_in(&snapshot, peer(&snapshot, SHELL_A));
        assert_eq!(pane_of(&lineage, &panes()), Some(7));
    }

    /// A process that left its tree hangs off init. Its chain names no pane,
    /// and that is a refusal: it is not treated as the pane it started in.
    #[test]
    fn a_detached_process_is_in_no_pane() {
        let snapshot = table();
        let lineage = lineage_in(&snapshot, peer(&snapshot, 900));
        assert_eq!(lineage, CallerLineage::Chain(vec![900, INIT]));
        assert_eq!(pane_of(&lineage, &panes()), None);
    }

    /// The app's own process is above every pane, not in one.
    #[test]
    fn a_process_above_the_panes_is_in_no_pane() {
        let snapshot = table();
        let lineage = lineage_in(&snapshot, peer(&snapshot, APP));
        assert_eq!(pane_of(&lineage, &panes()), None);
    }

    /// The pid was reused between the connection and the request: the number
    /// is in a pane, the process that connected is not that process.
    #[test]
    fn a_pid_that_changed_hands_is_not_placed() {
        let snapshot = table();
        let mut stale = peer(&snapshot, 112);
        stale.start = stale.start.map(|start| start + 1);
        let lineage = lineage_in(&snapshot, stale);
        assert_eq!(lineage, CallerLineage::Unknown);
        assert_eq!(pane_of(&lineage, &panes()), None);
    }

    #[test]
    fn a_caller_with_no_start_token_is_not_placed() {
        let snapshot = table();
        let lineage = lineage_in(
            &snapshot,
            Peer {
                pid: 112,
                start: None,
            },
        );
        assert_eq!(lineage, CallerLineage::Unknown);
    }

    #[test]
    fn a_pid_the_table_does_not_list_is_not_placed() {
        let snapshot = table();
        let lineage = lineage_in(
            &snapshot,
            Peer {
                pid: 4242,
                start: Some(1),
            },
        );
        assert_eq!(lineage, CallerLineage::Unknown);
    }

    #[test]
    fn nothing_is_placed_when_nobody_asked() {
        assert_eq!(pane_of(&CallerLineage::NotAsked, &panes()), None);
        assert_eq!(pane_of(&CallerLineage::Unknown, &panes()), None);
    }

    /// The nearest pane wins: a second instance of the app started inside a
    /// pane has panes of its own, and their shells are below the outer one.
    #[test]
    fn the_nearest_pane_wins() {
        let lineage = CallerLineage::Chain(vec![112, 111, 110, SHELL_A, APP, INIT]);
        let nested = HashMap::from([(SHELL_A, 7u64), (110, 9u64)]);
        assert_eq!(pane_of(&lineage, &nested), Some(9));
    }

    #[test]
    fn this_process_is_found_in_the_real_table() {
        let me = std::process::id();
        let peer = Peer::connected(Some(i64::from(me)));
        let lineage = lineage_now(peer);
        // A platform this build cannot read a start token on answers
        // `Unknown`, which is the refusal the rule asks for. The two this
        // test runs on can.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(matches!(lineage, CallerLineage::Chain(_)), "{lineage:?}");
        assert_ne!(lineage, CallerLineage::NotAsked);
        if let CallerLineage::Chain(chain) = lineage {
            assert_eq!(chain.first(), Some(&me));
        }
    }
}
