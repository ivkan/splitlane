//! A paired phone, from this machine's side: pairing from the terminal and
//! the bridge that answers the phone through the relay.
//!
//! The protocol is `splitlane_remote`'s; this module moves its frames and
//! answers the phone's requests with what the CLI already answers: through
//! the window when it is running, through the session host when it is not.

mod bridge;
mod journal;
mod pair;
mod relay;
mod spawn;
mod store;
mod surface;
mod window_pair;

use std::io;
use std::time::Duration;

use splitlane_remote::wire::Frame;

pub(crate) use bridge::serve;
pub(crate) use pair::CODE_ATTEMPTS;
pub(crate) use pair::pair;
pub(crate) use spawn::{
    BRIDGE_SUBCOMMAND, TOKEN_PARAM, keep_host_bridge, mark_bridge_request, new_token,
    run_spawned_bridge, start_bridge_if_paired, verified_device, wait_to_start_bridge,
};
pub(crate) use window_pair::{PairStep, WindowPairing};

/// A paired phone, as the Phone page lists it.
pub(crate) use store::Device as PairedPhone;

fn store_dir() -> Result<std::path::PathBuf, String> {
    store::dir().ok_or_else(|| "cannot work out where Splitlane keeps its files".to_string())
}

/// The phones paired with this computer. Reads a file: not on the main
/// thread.
pub(crate) fn paired_phones() -> Result<Vec<PairedPhone>, String> {
    store::devices(&store_dir()?).map_err(|e| e.to_string())
}

/// Forget the phone paired in `room`. The bridge leaves its room, and the
/// phone, which can no longer be answered, has to be paired again.
pub(crate) fn remove_phone(room: &str) -> Result<(), String> {
    store::remove_device(&store_dir()?, room).map_err(|e| e.to_string())
}

/// Let the phone paired in `room` reply, or only read.
pub(crate) fn set_phone_reply(room: &str, reply: bool) -> Result<(), String> {
    store::set_reply(&store_dir()?, room, reply).map_err(|e| e.to_string())
}

/// Something frames go in and out of: the relay, or a pair of queues in a
/// test.
pub(crate) trait Link {
    fn send(&mut self, frame: &Frame) -> io::Result<()>;
    fn receive(&mut self, wait: Duration) -> io::Result<relay::Received>;
    /// Keep a quiet connection open. A link that needs nothing does nothing.
    fn keep_alive(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Link for relay::RelaySocket {
    fn send(&mut self, frame: &Frame) -> io::Result<()> {
        relay::RelaySocket::send(self, frame)
    }

    fn receive(&mut self, wait: Duration) -> io::Result<relay::Received> {
        relay::RelaySocket::receive(self, wait)
    }

    fn keep_alive(&mut self) -> io::Result<()> {
        self.ping()
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod testing {
    //! A link made of two queues, for the bridge and the pairing tests.

    use std::io;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
    use std::time::Duration;

    use splitlane_remote::wire::Frame;

    use super::Link;
    use super::relay::Received;

    pub(crate) struct QueueLink {
        out: Sender<Vec<u8>>,
        inbox: Receiver<Vec<u8>>,
    }

    /// Two ends that see what the other sends, as the relay makes them.
    pub(crate) fn pair() -> (QueueLink, QueueLink) {
        let (a_out, b_in) = channel();
        let (b_out, a_in) = channel();
        (
            QueueLink {
                out: a_out,
                inbox: a_in,
            },
            QueueLink {
                out: b_out,
                inbox: b_in,
            },
        )
    }

    impl Link for QueueLink {
        fn send(&mut self, frame: &Frame) -> io::Result<()> {
            self.out
                .send(frame.encode())
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn receive(&mut self, wait: Duration) -> io::Result<Received> {
            match self.inbox.recv_timeout(wait) {
                Ok(bytes) => Ok(match Frame::decode(&bytes) {
                    Ok(frame) => Received::Frame(frame),
                    Err(_) => Received::Junk,
                }),
                Err(RecvTimeoutError::Timeout) => Ok(Received::Quiet),
                Err(RecvTimeoutError::Disconnected) => {
                    Err(io::Error::from(io::ErrorKind::ConnectionAborted))
                }
            }
        }
    }
}
