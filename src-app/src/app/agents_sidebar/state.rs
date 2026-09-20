//! State types for the Agents-mode sidebar affordances.
//!
//! Lives in its own submodule so the public surface visible from
//! `SplitlaneApp` is narrow and discoverable: three small enums that
//! discriminate "is this a project or a thread row?" for each
//! transient interaction. Cloning all three is free (small tags + two
//! `usize`s + an optional `Point<Pixels>`), so we pass them by value.

use gpui::{Pixels, Point};

/// Identifies a sidebar row that is currently in inline-rename mode.
/// Both projects and threads are renameable -- threads were originally
/// not (rely on agent-pushed `SessionInfoUpdate.title` / client auto-
/// derive) but Codex doesn't push titles and the background summarizer
/// is best-effort, so the user gets the always-works escape hatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentsRenameTarget {
    Project { ws_idx: usize },
    Thread { ws_idx: usize, thread_idx: usize },
}

/// Open right-click context menu, with the anchor position so the
/// deferred renderer can clamp to the window bounds.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AgentsContextMenu {
    Thread {
        ws_idx: usize,
        thread_idx: usize,
        position: Point<Pixels>,
        origin: MenuOrigin,
    },
    /// The `+ agent` menu at the foot of a container: which agent to start,
    /// and whether to remember it. An action lives where its object lives - a
    /// session belongs to a container, so the choice lives inside that container's rows, never at
    /// the rail's head.
    NewAgent {
        ws_idx: usize,
        position: Point<Pixels>,
    },
}

/// Where a surface's menu was opened from.
///
/// The rail's row and the slot header's `⋯` open the same menu, because they
/// name the same surface - but they do not stand for the same object. The row
/// stands for the surface *record*: pin it, rename it, duplicate it, delete
/// it, all of which survive the slot being closed. The header stands for what
/// is *on screen right now*, which is the only place a conversation exists to
/// be copied out of. Entries of the second kind are drawn only for
/// [`MenuOrigin::SlotHeader`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuOrigin {
    /// The surface's row in the rail, or its `⋯`.
    RailRow,
    /// The `⋯` in the header of the slot showing that surface.
    SlotHeader,
}

/// Pending delete confirmation. The mutation runs only after the user
/// clicks "Delete" in the confirmation dialog -- "Cancel" or
/// click-outside leaves the row intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentsDeleteTarget {
    Project {
        ws_idx: usize,
    },
    /// A surface, **by id**. The confirmation stands between the click and the
    /// deletion, and a pair of indices captured before it is a claim about a
    /// list that can move underneath: an agent exiting and being swept, a
    /// delete from another pane, and the row named at confirm time is not the
    /// row the dialog described. The id names one surface for as long as it
    /// exists.
    Thread {
        thread_id: u64,
    },
}
