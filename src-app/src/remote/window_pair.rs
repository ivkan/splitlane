//! Pairing a phone from the window: the same exchange as `remote pair`, with
//! the code typed into the Phone page instead of the terminal.
//!
//! The exchange blocks on the relay and on the person, so it runs on a
//! thread of its own and reports each step through a callback. The window
//! answers with the code it was shown, or ends the pairing by dropping the
//! handle.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use splitlane_remote::Offer;
use splitlane_remote::wire::Frame;

use super::pair::{CODE_ATTEMPTS, OFFER_TTL_SECS, PAGE, Unpaired, pair_on};
use super::relay::{self, Received, RelaySocket};
use super::{Link, now_unix, store};

/// Where a pairing from the window stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairStep {
    /// Joining the relay; there is nothing to show yet.
    Connecting,
    /// The link to show as a QR code, until the offer expires.
    Showing {
        link: String,
        expires_unix: u64,
    },
    /// A phone answered and shows its code. `wrong` is how many codes typed
    /// so far did not match; at `CODE_ATTEMPTS` the pairing ends.
    Asking {
        phone: String,
        wrong: usize,
    },
    Paired {
        phone: String,
    },
    Failed {
        reason: String,
    },
}

/// A pairing in progress. Dropping it ends the pairing: the phone is told
/// no code was entered, or, before a phone answered, the room is left.
pub(crate) struct WindowPairing {
    codes: mpsc::Sender<String>,
    cancelled: Arc<AtomicBool>,
}

impl WindowPairing {
    /// Start pairing, reporting every step to `report` from the pairing's
    /// own thread. Fails only when the thread cannot be started.
    pub(crate) fn start(report: impl Fn(PairStep) + Send + 'static) -> Result<Self, String> {
        let (codes, typed) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let spawned = std::thread::Builder::new()
            .name("phone-pairing".into())
            .spawn(move || {
                // A step after the window gave up is not news to anyone, and
                // must not land on a pairing started since.
                let report = |step| {
                    if !stop.load(Ordering::Relaxed) {
                        report(step);
                    }
                };
                let step = run(&typed, &stop, &report);
                report(step);
            });
        spawned.map_err(|e| format!("cannot start pairing: {e}"))?;
        Ok(Self { codes, cancelled })
    }

    /// The code the person typed.
    pub(crate) fn submit(&self, code: String) {
        let _ = self.codes.send(code);
    }
}

impl Drop for WindowPairing {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// The pairing, start to end; returns the last step.
fn run(
    typed: &mpsc::Receiver<String>,
    cancelled: &AtomicBool,
    report: &dyn Fn(PairStep),
) -> PairStep {
    let failed = |reason: String| PairStep::Failed { reason };
    let Some(dir) = store::dir() else {
        return failed("cannot work out where Splitlane keeps its files".into());
    };
    report(PairStep::Connecting);
    let key = match store::machine_key(&dir) {
        Ok(key) => key,
        Err(e) => return failed(format!("the computer's key: {e}")),
    };
    let offer = Offer::new(&key.public(), now_unix(), OFFER_TTL_SECS);
    let socket = match RelaySocket::connect(&relay::relay_url(), offer.room, &key) {
        Ok(socket) => socket,
        Err(e) => return failed(format!("cannot reach the relay: {e}")),
    };
    let mut link = Cancellable {
        inner: socket,
        cancelled,
    };
    report(PairStep::Showing {
        link: offer.to_link(PAGE),
        expires_unix: offer.expires_unix,
    });
    let mut wrong = 0;
    let mut ask_code = |phone: &str| {
        // Asked again means the last code did not match.
        report(PairStep::Asking {
            phone: phone.to_string(),
            wrong,
        });
        wrong += 1;
        // A code typed just before Cancel is still in the channel; the
        // person cancelled, so it pairs nothing.
        wait_for_code(typed, &offer).filter(|_| !cancelled.load(Ordering::Relaxed))
    };
    let mut keep = |asked: &splitlane_remote::wire::PairRequest| {
        // The last moment to notice a Cancel: nothing is kept after it.
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "pairing cancelled",
            ));
        }
        store::add_device(
            &dir,
            store::Device::new(offer.room, &asked.name, &asked.phone, now_unix()),
        )
    };
    let outcome = pair_on(&mut link, &key, &offer, &now_unix, &mut ask_code, &mut keep);
    // The phone's answer is the last frame sent; see `RelaySocket::close`.
    link.inner.close();
    match outcome {
        Ok(Ok(asked)) => {
            super::spawn::wake_bridge_starter();
            PairStep::Paired { phone: asked.name }
        }
        Ok(Err(Unpaired::Expired)) => failed("the link expired before a phone answered".into()),
        Ok(Err(Unpaired::WrongCode)) => failed(format!(
            "{CODE_ATTEMPTS} codes did not match; nothing was paired"
        )),
        // Waiting for a code ends at the offer's expiry too.
        Ok(Err(Unpaired::NoCode)) if offer.expired(now_unix()) => {
            failed("the five minutes ran out before a code was entered".into())
        }
        Ok(Err(Unpaired::NoCode)) => failed("no code was entered; nothing was paired".into()),
        Err(e) => failed(format!("pairing ended: {e}")),
    }
}

/// The next code typed, or `None` once the window gave up or the offer ran
/// out.
fn wait_for_code(typed: &mpsc::Receiver<String>, offer: &Offer) -> Option<String> {
    let left = offer.expires_unix.saturating_sub(now_unix());
    typed.recv_timeout(Duration::from_secs(left)).ok()
}

/// A link that fails once the window has given up, so a pairing waiting for
/// a phone that never comes does not hold its room open for the rest of
/// the five minutes.
struct Cancellable<'a, L> {
    inner: L,
    cancelled: &'a AtomicBool,
}

impl<L: Link> Link for Cancellable<'_, L> {
    fn send(&mut self, frame: &Frame) -> io::Result<()> {
        self.inner.send(frame)
    }

    fn receive(&mut self, wait: Duration) -> io::Result<Received> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "pairing cancelled",
            ));
        }
        self.inner.receive(wait)
    }

    fn keep_alive(&mut self) -> io::Result<()> {
        self.inner.keep_alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::testing;

    #[test]
    fn a_cancelled_link_stops_receiving() {
        let (machine_end, _phone_end) = testing::pair();
        let cancelled = AtomicBool::new(false);
        let mut link = Cancellable {
            inner: machine_end,
            cancelled: &cancelled,
        };
        assert!(matches!(
            link.receive(Duration::from_millis(1)),
            Ok(Received::Quiet)
        ));
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            link.receive(Duration::from_millis(1))
                .err()
                .map(|e| e.kind()),
            Some(io::ErrorKind::Interrupted)
        );
    }

    #[test]
    fn no_code_comes_once_the_window_has_gone() {
        let (codes, typed) = mpsc::channel::<String>();
        drop(codes);
        let offer = Offer::new(
            &splitlane_remote::DeviceKey::generate().public(),
            now_unix(),
            300,
        );
        assert_eq!(wait_for_code(&typed, &offer), None);
    }
}
