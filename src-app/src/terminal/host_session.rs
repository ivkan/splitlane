//! A pane whose terminal runs in the session host.
//!
//! The shell of such a pane is started by the host, not by the window. Its
//! output arrives over the host's socket instead of from a PTY the window
//! owns, and what the window has to say - input, a new size - goes back the
//! same way. The emulator, the view and the renderer are the ones every
//! other pane uses; only the two ends of the byte stream differ.
//!
//! This file is the window's side of that connection. It imports nothing of
//! the terminal emulator: what touches the emulator is in `pty_session`.

#![cfg_attr(
    not(test),
    allow(dead_code, reason = "no pane is opened in the session host yet")
)]

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

/// What a pane has to tell its session in the host.
///
/// Queued, and sent by a thread of the pane's own. The thread that calls
/// is the one that draws the window, and a request to the host waits for
/// its reply.
pub(super) enum RemoteMsg {
    Input(Cow<'static, [u8]>),
    /// The size of the grid, and of the whole text area in pixels. Zero
    /// pixels when the size of a cell is not known yet.
    Resize {
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    /// The pane is gone. Nothing queued behind this is sent.
    Shutdown,
}

/// The sending end of a pane's queue to the host.
#[derive(Clone)]
pub(super) struct RemoteSender {
    queue: Sender<RemoteMsg>,
    /// Bytes of input queued and not yet taken by the sending thread.
    queued_input: Arc<AtomicUsize>,
    input_limit: usize,
}

/// The receiving end, held by the thread that talks to the host.
pub(super) struct RemoteReceiver {
    queue: Receiver<RemoteMsg>,
    queued_input: Arc<AtomicUsize>,
}

/// A queue from a pane to its session. `input_limit` bounds the input that
/// may wait in it.
///
/// The queue itself has no bound, because a resize or a shutdown must never
/// be refused. Input is the only thing that can pile up: a program that
/// stops reading holds up the host's end of the connection, and everything
/// typed or pasted after that would otherwise be kept here for ever.
pub(super) fn remote_queue(input_limit: usize) -> (RemoteSender, RemoteReceiver) {
    let (queue_tx, queue_rx) = mpsc::channel();
    let queued_input = Arc::new(AtomicUsize::new(0));
    (
        RemoteSender {
            queue: queue_tx,
            queued_input: queued_input.clone(),
            input_limit,
        },
        RemoteReceiver {
            queue: queue_rx,
            queued_input,
        },
    )
}

impl RemoteSender {
    /// Queue input. False when it was refused because too much is waiting
    /// already; the caller counts that, as it does for input refused before
    /// the pane had a shell at all.
    pub(super) fn input(&self, bytes: Cow<'static, [u8]>) -> bool {
        let len = bytes.len();
        let reserved =
            self.queued_input
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                    queued
                        .checked_add(len)
                        .filter(|total| *total <= self.input_limit)
                });
        if reserved.is_err() {
            return false;
        }
        if self.queue.send(RemoteMsg::Input(bytes)).is_err() {
            // The sending thread is gone, which means the session is.
            // Input for it is dropped, as the host drops it.
            self.queued_input.fetch_sub(len, Ordering::AcqRel);
        }
        true
    }

    /// Queue anything that is not input. Never refused.
    pub(super) fn send(&self, msg: RemoteMsg) {
        let _ = self.queue.send(msg);
    }
}

impl RemoteReceiver {
    /// Wait for the next message. `None` once every sender is gone.
    pub(super) fn recv(&self) -> Option<RemoteMsg> {
        self.queue.recv().ok().map(|msg| self.taken(msg))
    }

    /// The next message if one is waiting.
    pub(super) fn try_recv(&self) -> Option<RemoteMsg> {
        match self.queue.try_recv() {
            Ok(msg) => Some(self.taken(msg)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    fn taken(&self, msg: RemoteMsg) -> RemoteMsg {
        if let RemoteMsg::Input(bytes) = &msg {
            self.queued_input.fetch_sub(bytes.len(), Ordering::AcqRel);
        }
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_waits_up_to_the_limit_and_is_refused_beyond_it() {
        let (sender, receiver) = remote_queue(10);
        assert!(sender.input(b"12345678".as_slice().into()));
        // Would make twelve.
        assert!(!sender.input(b"abcd".as_slice().into()));
        assert!(sender.input(b"ab".as_slice().into()));
        assert!(!sender.input(b"c".as_slice().into()));

        // What the sending thread has taken no longer counts.
        assert!(matches!(
            receiver.recv(),
            Some(RemoteMsg::Input(bytes)) if bytes.as_ref() == b"12345678"
        ));
        assert!(sender.input(b"abcdefgh".as_slice().into()));
        assert!(matches!(
            receiver.try_recv(),
            Some(RemoteMsg::Input(bytes)) if bytes.as_ref() == b"ab"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Some(RemoteMsg::Input(bytes)) if bytes.as_ref() == b"abcdefgh"
        ));
        assert!(receiver.try_recv().is_none());
    }

    #[test]
    fn a_full_queue_still_takes_everything_that_is_not_input() {
        let (sender, receiver) = remote_queue(4);
        assert!(sender.input(b"full".as_slice().into()));
        assert!(!sender.input(b"x".as_slice().into()));
        sender.send(RemoteMsg::Resize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        });
        sender.send(RemoteMsg::Shutdown);
        assert!(matches!(receiver.recv(), Some(RemoteMsg::Input(_))));
        assert!(matches!(
            receiver.recv(),
            Some(RemoteMsg::Resize {
                cols: 80,
                rows: 24,
                ..
            })
        ));
        assert!(matches!(receiver.recv(), Some(RemoteMsg::Shutdown)));
    }

    #[test]
    fn input_for_a_session_that_is_gone_is_dropped_and_not_counted() {
        let (sender, receiver) = remote_queue(4);
        drop(receiver);
        // Accepted every time: nothing is waiting, because nothing can.
        for _ in 0..3 {
            assert!(sender.input(b"abcd".as_slice().into()));
        }
        sender.send(RemoteMsg::Shutdown);
    }
}
