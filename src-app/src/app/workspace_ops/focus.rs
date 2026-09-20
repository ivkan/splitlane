//! Focus-movement handlers for `SplitlaneApp`.
//!
//! Part of the workspace_ops decomposition - behaviour identical to
//! the pre-refactor `main.rs` implementation.

use gpui::{App, Context, Focusable, Window};

use crate::SplitlaneApp;
use crate::layout::{FocusDirection, FocusNav};
use crate::{
    FocusDown, FocusLeft, FocusPreviousPane, FocusRight, FocusUp, JumpNextWaiting, SWAP_MODE,
};

/// Which surface takes input right now, named the way the rail names surfaces.
///
/// This is deliberately NOT `agents_target`. The selection answers "what is the
/// content area showing", stays the app's carrying model, and keeps deciding
/// what is drawn. Focus is the finer state *inside* that answer: which of the
/// container's one-to-three slots the keystrokes land in. The two coincide
/// whenever a parked surface is shown full-area - there is no other slot for
/// input to be in - and part ways the moment the panes are showing, which is
/// where the rail used to light every pane row at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FocusedSurface {
    /// An agent surface, wherever it is: parked and shown full-area, or living
    /// in a slot. It is one surface either way, and it has one rail row.
    Agent { ws_idx: usize, thread_id: u64 },
    /// A slot of the container's tree that shows no agent surface, by its leaf
    /// index counted left to right - which is exactly how the rail builds its
    /// pane rows.
    Slot { ws_idx: usize, leaf_idx: usize },
    /// The container's diff, shown full-area.
    Diff { ws_idx: usize },
}

impl SplitlaneApp {
    pub(crate) fn handle_focus(
        &mut self,
        dir: FocusDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // When swap mode is active, perform the swap instead of just moving focus
        if let Some(source) = self.swap_source.take() {
            SWAP_MODE.store(false, std::sync::atomic::Ordering::Relaxed);

            if let Some(ws) = self.active_workspace()
                && let Some(root) = &ws.root
            {
                // Move focus to find the target pane
                let moved = matches!(root.focus_in_direction(dir, window, cx), FocusNav::Moved);
                // focus-exact: focus was just moved by this keystroke, so the
                // live answer is the whole point - it names where the arrow
                // landed, which no remembered pane can.
                if let Some(target) = root.focused_pane(window, cx)
                    && target != source
                {
                    let swapped = if let Some(ws) = self.active_workspace_mut()
                        && let Some(ref mut root) = ws.root
                    {
                        root.swap_panes(&source, &target)
                    } else {
                        false
                    };
                    if swapped {
                        source.read(cx).focus_handle(cx).focus(window, cx);
                    } else {
                        self.show_toast("Swap source pane is no longer available", cx);
                    }
                } else if !moved {
                    self.show_toast("No pane in that direction", cx);
                }
            }
            self.save_session(cx);
            cx.notify();
            return;
        }

        if let Some(ws) = self.active_workspace()
            && let Some(root) = &ws.root
            && !matches!(root.focus_in_direction(dir, window, cx), FocusNav::Moved)
        {
            self.show_toast("No pane in that direction", cx);
        }
        cx.notify();
    }

    pub(crate) fn handle_focus_left(
        &mut self,
        _: &FocusLeft,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.handle_focus(FocusDirection::Left, w, cx);
    }
    pub(crate) fn handle_focus_right(
        &mut self,
        _: &FocusRight,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.handle_focus(FocusDirection::Right, w, cx);
    }
    pub(crate) fn handle_focus_up(&mut self, _: &FocusUp, w: &mut Window, cx: &mut Context<Self>) {
        self.handle_focus(FocusDirection::Up, w, cx);
    }
    pub(crate) fn handle_focus_down(
        &mut self,
        _: &FocusDown,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.handle_focus(FocusDirection::Down, w, cx);
    }

    /// Record which pane holds focus, keeping the one before it.
    ///
    /// Called once per frame from `render`. Focus resting anywhere that is not
    /// a pane - the rail, the palette, a dialog - leaves the pair alone: the
    /// design's chord returns to the previous *pane*, and a trip through the
    /// palette is not a pane visit.
    pub(crate) fn track_pane_focus(&mut self, window: &Window, cx: &gpui::App) {
        let Some(current) = self
            .active_workspace()
            .and_then(|ws| ws.root.as_ref())
            // focus-exact: this is what *maintains* the fallback. Asking the
            // fallback here would be circular, and a visit to the rail would
            // record itself as a pane visit.
            .and_then(|root| root.focused_pane(window, cx))
        else {
            return;
        };
        if self
            .focused_pane_now
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .is_some_and(|now| now == current)
        {
            return;
        }
        self.focused_pane_before = self.focused_pane_now.take();
        self.focused_pane_now = Some(current.downgrade());
    }

    /// The surface taking input, or `None` when nothing that has a rail row
    /// does - the launcher, the Skills page, Settings.
    ///
    /// Read once per frame by the rail, which is the only caller that needs
    /// the answer for every row at once.
    pub(crate) fn focused_surface(&self, window: &Window, cx: &App) -> Option<FocusedSurface> {
        // The application layer takes the whole window and belongs to no
        // container - `render` checks it before the selection, so this does.
        if self.settings_section.is_some() {
            return None;
        }
        self.focused_slot(window, cx)
    }

    /// **The pane the screen says is focused**, which is not the same question
    /// as GPUI's own.
    ///
    /// `LayoutTree::focused_pane` asks the window: is this pane's handle the
    /// focused one *right now*. That is exact and often `None` - the rail, the
    /// palette, a dialog, an inline rename, the Files tree all hold focus
    /// themselves. The app's answer is the three-step one below, and it is the
    /// one **drawn**: the pane border, the slot header's fill and the rail
    /// row's accent bar are all painted from it, so at any moment a person can
    /// point at the pane this returns.
    ///
    /// So an action that needs "the pane you are in" asks this, not the window.
    /// `split` asked the window and gave up, which is how `Add pane` came to do
    /// nothing after a click on the rail - and why clicking another project and
    /// coming back fixed it, since selecting a container calls `focus_first`.
    /// Found in use.
    ///
    /// `None` means the container genuinely has no panes, which is a state this
    /// app has.
    pub(crate) fn focused_pane_as_shown(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<gpui::Entity<crate::pane::Pane>> {
        let root = self.workspaces.get(self.active_idx)?.root.as_ref()?;
        let leaves = root.collect_leaves();
        // Live focus first; then the pane focus came from, which is the pair
        // the previous-pane chord is answered from.
        // focus-exact: the first of the three steps, inside the helper the
        // rule points at.
        root.focused_pane(window, cx)
            .or_else(|| {
                self.focused_pane_now
                    .as_ref()
                    .and_then(|w| w.upgrade())
                    .filter(|pane| leaves.contains(pane))
            })
            // "Exactly one pane is focused at all times, `focusIndex`. There is
            // no 'nothing focused' state." Between a container switch and the
            // frame that hands the keyboard over there is a moment when GPUI's
            // focus is on a pane of the container just left, and the remembered
            // pane is that same one - the first pane of what is on screen is
            // the answer both are about to become.
            .or_else(|| leaves.first().cloned())
    }

    /// Give the keyboard back to the pane the screen already names.
    ///
    /// A surface that took focus and then went has to hand it back, because
    /// GPUI dispatches an action along the **focus chain**: a handle that is
    /// not in the rendered frame's dispatch tree falls back to the window root,
    /// which sits *above* this app's own root `div` - and every `on_action`
    /// lives on that `div`. So a stale focus does not misroute one keystroke,
    /// it kills every action in the app at once, chord and button alike.
    ///
    /// The pane it hands to is [`Self::focused_pane_as_shown`], which is what
    /// the border, the slot header and the rail's accent bar are already
    /// drawing - so the keyboard lands where the screen says the user is. A
    /// container with no panes at all has the empty state instead, which is the
    /// one thing on screen there.
    pub(crate) fn return_focus_to_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focused_pane_as_shown(window, cx) {
            Some(pane) => pane.read(cx).focus_handle(cx).focus(window, cx),
            None => self.empty_panes_focus.focus(window, cx),
        }
    }

    /// The slot of the active container that takes input, resolved to whatever
    /// surface is active in it.
    fn focused_slot(&self, window: &Window, cx: &App) -> Option<FocusedSurface> {
        let ws_idx = self.active_idx;
        let root = self.workspaces.get(ws_idx)?.root.as_ref()?;
        let leaves = root.collect_leaves();
        // Without the fallback the accent bar would go out the instant the user
        // clicked the rail, which is the one moment they are looking at it.
        let focused = self.focused_pane_as_shown(window, cx)?;
        let leaf_idx = leaves.iter().position(|p| *p == focused)?;
        // The slot's *active* tab is what input reaches; an agent in a
        // background tab of the focused slot is open, not focused. The same
        // rule answers for the diff, which is a tab like any other now: the
        // rail draws it as the container's `Changes` row, so the focused
        // surface has to be named that way and not by leaf index.
        let pane = focused.read(cx);
        if matches!(
            pane.tabs.get(pane.selected_idx),
            Some(crate::pane::TabContent::Diff(_))
        ) {
            return Some(FocusedSurface::Diff { ws_idx });
        }
        let thread_id = pane
            .active_terminal_opt()
            .and_then(|view| view.read(cx).agent_thread_id);
        Some(match thread_id {
            Some(thread_id) => FocusedSurface::Agent { ws_idx, thread_id },
            None => FocusedSurface::Slot { ws_idx, leaf_idx },
        })
    }

    /// The design's `\u{2303}\u{21e5}`: back to the pane focus came from.
    ///
    /// Silent when there is nowhere to go back to - a first pane, a pane that
    /// has since closed, or a previous pane that now lives in another
    /// container. A toast would fire on the very first press of a chord whose
    /// whole point is speed.
    pub(crate) fn handle_focus_previous_pane(
        &mut self,
        _: &FocusPreviousPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(previous) = self
            .focused_pane_before
            .as_ref()
            .and_then(|weak| weak.upgrade())
        else {
            return;
        };
        let still_here = self
            .active_workspace()
            .and_then(|ws| ws.root.as_ref())
            .is_some_and(|root| root.contains_leaf(&previous));
        if !still_here {
            return;
        }
        previous.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Teleport to the next agent that is
    /// `WaitingForInput`, cross-container, in the rail's own order
    /// (container, then its surfaces, then its panes). Repeated presses cycle
    /// through the waiting set via `jump_cursor`; activating a background tab
    /// is part of the jump, since the waiting surface may be hidden. No
    /// waiting agent → silent no-op (an empty queue is the good news).
    ///
    /// The order comes from `waiting_stops` - the same list the title bar's
    /// chip counts and opens. It used to be a separate walk over
    /// `agent_sessions` resolved to panes, which could not reach an agent
    /// surface at all: those report through `Thread::status`, so the chip said
    /// "1 agent waiting" and the chord answered nothing.
    pub(crate) fn handle_jump_next_waiting(
        &mut self,
        _: &JumpNextWaiting,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stops = self.waiting_stops(cx);
        let keys: Vec<u64> = stops
            .iter()
            .filter_map(|stop| self.stop_key(stop, cx))
            .collect();
        let Some(next) = next_in_cycle(&keys, self.jump_cursor) else {
            return;
        };
        let Some(stop) = stops
            .into_iter()
            .find(|stop| self.stop_key(stop, cx) == Some(next))
        else {
            return;
        };
        self.go_to_stop(stop, window, cx);
        self.jump_cursor = Some(next);
    }
}

/// Pure cycle rule (unit-tested): first waiting surface when the cursor is
/// unset or gone from the set; otherwise the one after it, wrapping.
fn next_in_cycle(order: &[u64], last: Option<u64>) -> Option<u64> {
    if order.is_empty() {
        return None;
    }
    match last.and_then(|l| order.iter().position(|&x| x == l)) {
        Some(pos) => Some(order[(pos + 1) % order.len()]),
        None => Some(order[0]),
    }
}

#[cfg(test)]
mod tests {
    use super::next_in_cycle;

    #[test]
    fn empty_set_is_none() {
        assert_eq!(next_in_cycle(&[], None), None);
        assert_eq!(next_in_cycle(&[], Some(7)), None);
    }

    #[test]
    fn unset_or_stale_cursor_starts_at_first() {
        assert_eq!(next_in_cycle(&[10, 20, 30], None), Some(10));
        // Cursor points at a surface that stopped waiting: restart at first.
        assert_eq!(next_in_cycle(&[10, 20, 30], Some(99)), Some(10));
    }

    #[test]
    fn cycles_and_wraps() {
        assert_eq!(next_in_cycle(&[10, 20, 30], Some(10)), Some(20));
        assert_eq!(next_in_cycle(&[10, 20, 30], Some(30)), Some(10));
        // Single waiting pane: jumping again stays on it.
        assert_eq!(next_in_cycle(&[10], Some(10)), Some(10));
    }
}
