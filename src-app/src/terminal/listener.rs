//! Alacritty event-listener bridge + grid dimensions adapter.
//!
//! `ZedListener` carries `AlacEvent`s from the VTE thread to the GPUI main
//! thread via a `futures::mpsc` channel. `SpikeTermSize` adapts our
//! `(columns, screen_lines)` pair to alacritty's `Dimensions` trait.

use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use futures::channel::mpsc::UnboundedSender;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::limits::MAX_OSC52_BYTES;

pub struct SpikeTermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for SpikeTermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

#[derive(Default)]
pub(super) struct ClipboardGate {
    state: AtomicU8,
}

impl ClipboardGate {
    const FOCUSED: u8 = 1 << 0;
    const STORE_ALLOWED: u8 = 1 << 1;
    const LOAD_ALLOWED: u8 = 1 << 2;

    pub(super) fn set_focused(&self, focused: bool) {
        if focused {
            self.state.fetch_or(Self::FOCUSED, Ordering::AcqRel);
        } else {
            self.state.fetch_and(!Self::FOCUSED, Ordering::AcqRel);
        }
    }

    pub(super) fn set_policy(&self, store_allowed: bool, load_allowed: bool) {
        let mut policy = 0;
        if store_allowed {
            policy |= Self::STORE_ALLOWED;
        }
        if load_allowed {
            policy |= Self::LOAD_ALLOWED;
        }
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                Some((state & Self::FOCUSED) | policy)
            });
    }

    pub(super) fn allows_store(&self) -> bool {
        let required = Self::FOCUSED | Self::STORE_ALLOWED;
        self.state.load(Ordering::Acquire) & required == required
    }

    fn allows_load(&self) -> bool {
        let required = Self::FOCUSED | Self::LOAD_ALLOWED;
        self.state.load(Ordering::Acquire) & required == required
    }
}

/// Which of the emulator's events a terminal lets through when its PTY is in
/// the session host.
///
/// The emulator answers the program by emitting events: a reply to write to
/// the PTY, a color or a size to fill in. For a PTY the window owns, those
/// are the answers. For one the host owns the host has already answered,
/// from an emulator of its own that read the same bytes, and a second
/// answer from here would reach the program as input it never asked for.
///
/// Replaying a session's log needs more held back. Everything in it has
/// happened already: a bell that rang, a copy to the clipboard that was
/// made, a paste that was asked for. Of the titles the log sets only the
/// last one matters, so the others are dropped and that one is kept for the
/// end of the replay.
///
/// One gate per terminal, set where the terminal is given its PTY. Every
/// terminal starts as [`RemoteGate::LOCAL`], which changes nothing.
#[cfg(target_os = "macos")]
#[derive(Default)]
pub(super) struct RemoteGate {
    mode: AtomicU8,
    /// The newest title set during a replay: `Some(None)` for a reset to
    /// the default.
    held_title: std::sync::Mutex<Option<Option<String>>>,
}

#[cfg(target_os = "macos")]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "no pane is opened in the session host yet")
)]
impl RemoteGate {
    /// The window owns the PTY and its emulator answers the program.
    pub(super) const LOCAL: u8 = 0;
    /// The host owns the PTY, and what the emulator reads is old output.
    pub(super) const REMOTE_REPLAY: u8 = 1;
    /// The host owns the PTY, and what the emulator reads is happening now.
    pub(super) const REMOTE_LIVE: u8 = 2;

    pub(super) fn set_mode(&self, mode: u8) {
        self.mode.store(mode, Ordering::Release);
    }

    pub(super) fn mode(&self) -> u8 {
        self.mode.load(Ordering::Acquire)
    }

    pub(super) fn is_replaying(&self) -> bool {
        self.mode() == Self::REMOTE_REPLAY
    }

    /// True when `event` must not reach the window. A title held back
    /// during a replay is remembered here on the way.
    fn holds_back(&self, event: &AlacEvent) -> bool {
        let mode = self.mode();
        if mode == Self::LOCAL {
            return false;
        }
        match event {
            AlacEvent::PtyWrite(_)
            | AlacEvent::ColorRequest(..)
            | AlacEvent::TextAreaSizeRequest(_) => true,
            _ if mode != Self::REMOTE_REPLAY => false,
            AlacEvent::ClipboardStore(..) | AlacEvent::ClipboardLoad(..) | AlacEvent::Bell => true,
            AlacEvent::Title(title) => {
                self.hold_title(Some(title.clone()));
                true
            }
            AlacEvent::ResetTitle => {
                self.hold_title(None);
                true
            }
            _ => false,
        }
    }

    fn hold_title(&self, title: Option<String>) {
        *self
            .held_title
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(title);
    }

    /// End a replay: from here on the output is live. Returns the title
    /// the replay left behind, as the event that sets it, for the caller
    /// to send now that it will be let through.
    pub(super) fn go_live(&self) -> Option<AlacEvent> {
        self.set_mode(Self::REMOTE_LIVE);
        let held = self
            .held_title
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()?;
        Some(match held {
            Some(title) => AlacEvent::Title(title),
            None => AlacEvent::ResetTitle,
        })
    }
}

#[derive(Clone)]
pub struct ZedListener {
    events: UnboundedSender<AlacEvent>,
    clipboard_gate: Option<Arc<ClipboardGate>>,
    #[cfg(target_os = "macos")]
    remote_gate: Option<Arc<RemoteGate>>,
}

impl ZedListener {
    #[cfg(test)]
    pub(super) fn new(events: UnboundedSender<AlacEvent>) -> Self {
        Self {
            events,
            clipboard_gate: None,
            #[cfg(target_os = "macos")]
            remote_gate: None,
        }
    }

    pub(super) fn with_clipboard_gate(
        events: UnboundedSender<AlacEvent>,
        clipboard_gate: Arc<ClipboardGate>,
    ) -> Self {
        Self {
            events,
            clipboard_gate: Some(clipboard_gate),
            #[cfg(target_os = "macos")]
            remote_gate: None,
        }
    }

    /// A listener for a terminal that may be given a PTY in the session
    /// host. See [`RemoteGate`].
    #[cfg(target_os = "macos")]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "no pane is opened in the session host yet")
    )]
    pub(super) fn with_gates(
        events: UnboundedSender<AlacEvent>,
        clipboard_gate: Arc<ClipboardGate>,
        remote_gate: Arc<RemoteGate>,
    ) -> Self {
        Self {
            events,
            clipboard_gate: Some(clipboard_gate),
            remote_gate: Some(remote_gate),
        }
    }
}

impl EventListener for ZedListener {
    fn send_event(&self, event: AlacEvent) {
        if let Some(gate) = &self.clipboard_gate {
            match &event {
                AlacEvent::ClipboardStore(_, text) if text.len() > MAX_OSC52_BYTES => return,
                AlacEvent::ClipboardStore(..) if !gate.allows_store() => return,
                AlacEvent::ClipboardLoad(..) if !gate.allows_load() => return,
                _ => {}
            }
        }
        #[cfg(target_os = "macos")]
        if self
            .remote_gate
            .as_ref()
            .is_some_and(|gate| gate.holds_back(&event))
        {
            return;
        }
        let _ = self.events.unbounded_send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::ClipboardType;
    use futures::channel::mpsc::unbounded;

    #[test]
    fn clipboard_events_are_filtered_at_the_vte_source() {
        let (events_tx, mut events_rx) = unbounded();
        let gate = Arc::new(ClipboardGate::default());
        let listener = ZedListener::with_clipboard_gate(events_tx, gate.clone());

        listener.send_event(AlacEvent::ClipboardStore(
            ClipboardType::Clipboard,
            "unfocused".into(),
        ));
        assert!(events_rx.try_recv().is_err());

        gate.set_policy(true, false);
        gate.set_focused(true);
        listener.send_event(AlacEvent::ClipboardStore(
            ClipboardType::Clipboard,
            "focused".into(),
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(AlacEvent::ClipboardStore(_, text)) if text == "focused"
        ));

        listener.send_event(AlacEvent::ClipboardStore(
            ClipboardType::Clipboard,
            "x".repeat(MAX_OSC52_BYTES + 1),
        ));
        assert!(events_rx.try_recv().is_err());

        gate.set_focused(false);
        listener.send_event(AlacEvent::ClipboardStore(
            ClipboardType::Clipboard,
            "lost-focus".into(),
        ));
        assert!(events_rx.try_recv().is_err());
    }

    /// One of each event the gate has an opinion on, and two it has none
    /// on. The clipboard events would pass the clipboard gate.
    #[cfg(target_os = "macos")]
    fn one_of_each() -> Vec<(&'static str, AlacEvent)> {
        use alacritty_terminal::event::WindowSize;
        use alacritty_terminal::vte::ansi::Rgb;

        vec![
            ("pty-write", AlacEvent::PtyWrite("\x1b[1;1R".into())),
            (
                "color",
                AlacEvent::ColorRequest(257, Arc::new(|_: Rgb| String::new())),
            ),
            (
                "size",
                AlacEvent::TextAreaSizeRequest(Arc::new(|_: WindowSize| String::new())),
            ),
            (
                "store",
                AlacEvent::ClipboardStore(ClipboardType::Clipboard, "copied".into()),
            ),
            (
                "load",
                AlacEvent::ClipboardLoad(
                    ClipboardType::Clipboard,
                    Arc::new(|_: &str| String::new()),
                ),
            ),
            ("bell", AlacEvent::Bell),
            ("title", AlacEvent::Title("vim".into())),
            ("reset-title", AlacEvent::ResetTitle),
            ("wakeup", AlacEvent::Wakeup),
            ("blink", AlacEvent::CursorBlinkingChange),
        ]
    }

    #[cfg(target_os = "macos")]
    fn name_of(event: &AlacEvent) -> &'static str {
        match event {
            AlacEvent::PtyWrite(_) => "pty-write",
            AlacEvent::ColorRequest(..) => "color",
            AlacEvent::TextAreaSizeRequest(_) => "size",
            AlacEvent::ClipboardStore(..) => "store",
            AlacEvent::ClipboardLoad(..) => "load",
            AlacEvent::Bell => "bell",
            AlacEvent::Title(_) => "title",
            AlacEvent::ResetTitle => "reset-title",
            AlacEvent::Wakeup => "wakeup",
            AlacEvent::CursorBlinkingChange => "blink",
            _ => "other",
        }
    }

    /// What reaches the window in each of the three modes.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_terminal_in_the_session_host_keeps_its_own_answers_to_itself() {
        let passed = |mode: u8| {
            let (events_tx, mut events_rx) = unbounded();
            let clipboard = Arc::new(ClipboardGate::default());
            clipboard.set_policy(true, true);
            clipboard.set_focused(true);
            let remote = Arc::new(RemoteGate::default());
            remote.set_mode(mode);
            let listener = ZedListener::with_gates(events_tx, clipboard, remote);
            for (_, event) in one_of_each() {
                listener.send_event(event);
            }
            let mut names = Vec::new();
            while let Ok(event) = events_rx.try_recv() {
                names.push(name_of(&event));
            }
            names
        };

        let everything: Vec<_> = one_of_each().into_iter().map(|(name, _)| name).collect();
        assert_eq!(passed(RemoteGate::LOCAL), everything);
        // The host has answered the program; the rest is the window's.
        assert_eq!(
            passed(RemoteGate::REMOTE_LIVE),
            [
                "store",
                "load",
                "bell",
                "title",
                "reset-title",
                "wakeup",
                "blink"
            ]
        );
        // Old output: nothing that acts on the world a second time.
        assert_eq!(passed(RemoteGate::REMOTE_REPLAY), ["wakeup", "blink"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_replay_leaves_only_its_last_title_behind() {
        let (events_tx, mut events_rx) = unbounded();
        let remote = Arc::new(RemoteGate::default());
        let listener = ZedListener::with_gates(
            events_tx,
            Arc::new(ClipboardGate::default()),
            remote.clone(),
        );
        remote.set_mode(RemoteGate::REMOTE_REPLAY);
        assert!(remote.is_replaying());

        listener.send_event(AlacEvent::Title("first".into()));
        listener.send_event(AlacEvent::ResetTitle);
        listener.send_event(AlacEvent::Title("last".into()));
        assert!(events_rx.try_recv().is_err());

        assert!(matches!(remote.go_live(), Some(AlacEvent::Title(title)) if title == "last"));
        assert_eq!(remote.mode(), RemoteGate::REMOTE_LIVE);
        // Handed over once, and a title set from here on is not held.
        assert!(remote.go_live().is_none());
        listener.send_event(AlacEvent::Title("live".into()));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(AlacEvent::Title(title)) if title == "live"
        ));

        // A replay that ended on a reset says so.
        remote.set_mode(RemoteGate::REMOTE_REPLAY);
        listener.send_event(AlacEvent::Title("first".into()));
        listener.send_event(AlacEvent::ResetTitle);
        assert!(matches!(remote.go_live(), Some(AlacEvent::ResetTitle)));
        // And one that set no title leaves the one there was.
        remote.set_mode(RemoteGate::REMOTE_REPLAY);
        assert!(remote.go_live().is_none());
    }
}
