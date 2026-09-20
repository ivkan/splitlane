//! The macOS notification backend: `UNUserNotificationCenter`, asked before it
//! is trusted.
//!
//! **This exists because the previous backend was a diagnosis, not a
//! delivery.** `notify-rust`'s macOS arm is `mac-notification-sys`, which is
//! built exclusively on `NSUserNotificationCenter` - deprecated in 10.14 and
//! silently ignored since. Measured 7 September 2026: `show()` answered
//! `Ok(())`, and `splitlane` never appeared in the notification daemon's log at
//! all. Three of the four things this app has to say - "needs input",
//! "crashed", "stalled" - had therefore never been seen by anybody on macOS,
//! and they are the channel that works when nobody is looking at the app, which
//! is the case the product exists for.
//!
//! **Two measured facts shape every function here.**
//!
//! The API needs an `.app` bundle. An ad-hoc signature is enough (measured 8
//! September: a `codesign -s -` bundle in `/Applications` delivered, and the
//! system returned the delivered notification's id), so Developer ID is not on
//! this path - but a `cargo run` build has no bundle and will **never** show a
//! notification. That is a property of the platform API, not a bug to hunt.
//!
//! And `send` answers `ok` even when authorization is `Denied` - measured, same
//! day, on this machine. So the trap the old backend set ("`Ok` means
//! delivered") reproduces one floor up, and swapping the backend alone does not
//! cure it. Nothing here trusts a send: [`refresh`] asks the system what it
//! will actually do, [`permission`] is what Settings states out loud, and
//! [`deliver`] refuses rather than reporting a success it cannot vouch for.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use mac_usernotifications::{
    AuthorizationStatus, Notification, NotificationSettingStatus, blocking,
};

/// What macOS will do with a notification this process sends.
///
/// It is deliberately **not** a boolean, and not the OS enum either. Most of
/// these look like "off" to a person while being reached, escaped and explained
/// in entirely different ways - and two of them, [`Self::AllowedQuietly`] and
/// [`Self::AllowedNowhere`], read as "on" to every check the app could make
/// while putting little or nothing in front of the reader's eyes. Two more,
/// [`Self::Unchecked`] and [`Self::Unknown`], are not answers about macOS at
/// all but admissions about us, which is why [`Self::will_arrive`] has three
/// values rather than two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MacNotificationPermission {
    /// Nobody has looked yet. Not an answer about macOS - an answer about us.
    Unchecked,
    /// macOS has not been asked. Nothing is delivered, and the next agent
    /// session to start will ask.
    NotAsked,
    /// Allowed, and banners are on: a notification reaches the screen.
    Allowed,
    /// Allowed, and banners are off: it reaches Notification Center and
    /// nothing else.
    ///
    /// **This is the state that would rebuild the bug.** Authorization is
    /// `Authorized`, `send` answers `ok`, the system genuinely holds the
    /// notification - and the person waiting to be told an agent needs them
    /// sees nothing until they go looking, which is the one thing this channel
    /// exists to spare them. It is a separate word here because it is a
    /// separate thing to say.
    AllowedQuietly,
    /// Allowed, and turned off in both places it could show: no banner and no
    /// Notification Center entry either.
    ///
    /// **Not a pedantic third shade of `AllowedQuietly`.** That one still puts
    /// the notification somewhere a person can go and find it later; this one
    /// puts it nowhere at all, so it belongs on the closed side of the channel
    /// while reporting `authorizationStatus: Authorized` to every check. It is
    /// reachable in System Settings by setting the alert style to None and
    /// clearing "Show in Notification Center".
    AllowedNowhere,
    /// The person said no. `send` still answers `ok` and nothing arrives, and
    /// the app cannot ask again - only System Settings can undo it.
    Denied,
    /// There is no `.app` bundle, so the API is unusable. Every `cargo run`
    /// build is here, permanently and correctly.
    NoBundle,
    /// macOS answered something this build does not have a word for.
    Unknown,
}

impl MacNotificationPermission {
    /// Will a notification sent right now reach the person - **yes, no, or we
    /// do not know**?
    ///
    /// The third answer is the whole reason this is not a `bool`. Two states
    /// are genuine ignorance rather than a reading: [`Self::Unchecked`], where
    /// nobody has asked the system yet, and [`Self::Unknown`], where the system
    /// was asked and gave an answer this build has no word for. Collapsing
    /// either into `true` makes Settings claim `covered` on no evidence, which
    /// is the defect this module exists to stop; collapsing them into `false`
    /// makes it claim `blocked` on no evidence, which is the same defect
    /// pointing the other way.
    pub(crate) fn will_arrive(self) -> Option<bool> {
        match self {
            Self::Unchecked | Self::Unknown => None,
            Self::Allowed | Self::AllowedQuietly => Some(true),
            Self::NotAsked | Self::AllowedNowhere | Self::Denied | Self::NoBundle => Some(false),
        }
    }

    /// Is it worth handing macOS a notification at all?
    ///
    /// **Ignorance answers yes here, and that is deliberate rather than an
    /// oversight of [`Self::will_arrive`].** The two questions have different
    /// costs for the same wrong answer. A row that claims coverage it cannot
    /// prove tells a person something false, and they will act on it. A send
    /// into a channel that turns out to be shut costs a log line nobody was
    /// waiting on - while *not* sending, on the chance that the state we could
    /// not read was a refusal, costs the notification itself, and a missed
    /// notification is the failure this whole track was opened over. So the
    /// interface refuses to guess and the sender guesses in the cheap
    /// direction.
    pub(crate) fn channel_is_open(self) -> bool {
        self.will_arrive() != Some(false)
    }

    /// The word Settings states, and the sentence under it.
    pub(crate) fn stated(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Unchecked => ("checking", None),
            Self::NotAsked => (
                "not asked yet",
                Some("macOS is asked when the first agent session starts"),
            ),
            Self::Allowed => ("allowed", None),
            Self::AllowedQuietly => (
                "no banners",
                Some(
                    "allowed, but banners are off - notifications go to Notification Center \
                     and nothing appears on screen",
                ),
            ),
            Self::AllowedNowhere => (
                "off everywhere",
                Some(
                    "allowed, but both banners and Notification Center are off in System \
                     Settings, so a notification arrives nowhere at all",
                ),
            ),
            Self::Denied => (
                "denied",
                Some(
                    "macOS blocks every notification below, and only System Settings \u{2192} \
                     Notifications \u{2192} Splitlane can undo it",
                ),
            ),
            Self::NoBundle => (
                "unavailable",
                Some(
                    "this build is not running from a Splitlane.app bundle, and macOS \
                     delivers no notification without one",
                ),
            ),
            Self::Unknown => (
                "unknown",
                Some("macOS gave an answer this build cannot read"),
            ),
        }
    }

    /// Every state, in one place, so a test can walk them all rather than
    /// remember to be extended when a state is added.
    #[cfg(test)]
    const ALL: [Self; 8] = [
        Self::Unchecked,
        Self::NotAsked,
        Self::Allowed,
        Self::AllowedQuietly,
        Self::AllowedNowhere,
        Self::Denied,
        Self::NoBundle,
        Self::Unknown,
    ];

    fn as_byte(self) -> u8 {
        match self {
            Self::Unchecked => 0,
            Self::NotAsked => 1,
            Self::Allowed => 2,
            Self::AllowedQuietly => 3,
            Self::Denied => 4,
            Self::NoBundle => 5,
            Self::Unknown => 6,
            Self::AllowedNowhere => 7,
        }
    }

    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::NotAsked,
            2 => Self::Allowed,
            3 => Self::AllowedQuietly,
            4 => Self::Denied,
            5 => Self::NoBundle,
            6 => Self::Unknown,
            7 => Self::AllowedNowhere,
            _ => Self::Unchecked,
        }
    }
}

/// The last answer macOS gave, so the render thread never has to ask.
///
/// Every read of the system's settings crosses into Objective-C and blocks on a
/// completion handler, which is a thing a frame may not do. So the answer is
/// cached here by whoever asked last - the first agent session, a send, or the
/// Settings section opening - and the interface reads the cache.
static CACHED: AtomicU8 = AtomicU8::new(0);

/// Whether this process has already put the question to the person.
///
/// **Restore is why this is not merely tidy.** A container coming back with
/// three agent surfaces builds three of them in one pass, and each one reaches
/// [`request_if_never_asked`] on its own background thread - so without a claim
/// taken before the blocking call, one launch would put three authorization
/// requests to macOS at once. `NotDetermined` cannot serve as that claim: it
/// stays `NotDetermined` for the whole time the alert is on screen, which is
/// exactly the window the other two arrive in.
static ASKED: AtomicBool = AtomicBool::new(false);

/// What macOS said last time anybody asked. Free, and safe from a frame.
pub(crate) fn permission() -> MacNotificationPermission {
    MacNotificationPermission::from_byte(CACHED.load(Ordering::Relaxed))
}

/// Ask macOS what it will do, without prompting anybody.
///
/// **Blocking, and never to be called from the render thread.**
/// `getNotificationSettings` answers on one of Apple's own queues; this waits
/// for it. `getNotificationSettings` is also the whole reason the prompt does
/// not have to hang off app launch: the state can be *learnt* without being
/// *asked for*.
pub(crate) fn refresh() -> MacNotificationPermission {
    let resolved = match blocking::get_notification_settings() {
        Ok(settings) => match settings.authorization_status {
            AuthorizationStatus::NotDetermined => MacNotificationPermission::NotAsked,
            AuthorizationStatus::Denied => MacNotificationPermission::Denied,
            // **Authorized is not the same as visible, and it takes two
            // questions to find that out, not one.** The alert style and "Show
            // in Notification Center" are separate switches in System
            // Settings, and each one being off subtracts one of the only two
            // places a notification can end up. With both off, macOS reports
            // `Authorized`, accepts the request, answers `ok`, and puts the
            // notification nowhere - which is `Denied` in every way that
            // matters to a reader and in no way an API can be asked about
            // directly.
            AuthorizationStatus::Authorized | AuthorizationStatus::Ephemeral => {
                let banner = settings.alert_enabled == NotificationSettingStatus::Enabled;
                let centre =
                    settings.notification_center_enabled == NotificationSettingStatus::Enabled;
                match (banner, centre) {
                    (true, _) => MacNotificationPermission::Allowed,
                    (false, true) => MacNotificationPermission::AllowedQuietly,
                    (false, false) => MacNotificationPermission::AllowedNowhere,
                }
            }
            // Provisional is quiet **by definition** - macOS grants it without
            // asking precisely so notifications go to Notification Center and
            // no further - so the alert setting cannot promote it to a banner,
            // and reading it as `Allowed` would promise a banner nothing will
            // draw. Nothing in this app requests provisional authorization; it
            // is handled because the state can arrive from outside.
            AuthorizationStatus::Provisional => MacNotificationPermission::AllowedQuietly,
            // The crate's own catch-all for a status code it did not
            // recognise, so there is nothing left for a `_` arm to catch -
            // and a `_` here would silently swallow a variant a future
            // version adds rather than failing the build on it.
            AuthorizationStatus::Unknown => MacNotificationPermission::Unknown,
        },
        Err(mac_usernotifications::Error::NoBundleIdentifier) => {
            MacNotificationPermission::NoBundle
        }
        Err(err) => {
            log::warn!("macOS notification settings unavailable: {err}");
            MacNotificationPermission::Unknown
        }
    };
    CACHED.store(resolved.as_byte(), Ordering::Relaxed);
    resolved
}

/// Ask the person, but only if macOS has never been asked.
///
/// **Blocking for as long as the system alert is on screen**, so this belongs
/// on a background thread and nowhere else.
///
/// The `NotAsked` guard is the whole of the "ask once" bookkeeping, and it is
/// deliberately not a config key: `NotDetermined` *is* the record that nobody
/// has answered, macOS keeps it, and it survives reinstalls of this app's own
/// state. A second `requestAuthorization` after a refusal shows nothing and
/// quietly answers `false`, so the guard costs nothing in correctness either -
/// it is here so the log says what happened rather than to prevent a prompt.
pub(crate) fn request_if_never_asked() {
    if refresh() != MacNotificationPermission::NotAsked {
        return;
    }
    // Claim the question before asking it, so the other surfaces of a restored
    // container find it taken rather than each raising its own alert.
    if !claim_the_question(&ASKED) {
        return;
    }
    match blocking::request_auth() {
        Ok(true) => log::info!("macOS notifications: the person allowed them"),
        Ok(false) => log::info!(
            "macOS notifications: the person declined, or dismissed the prompt without answering"
        ),
        Err(err) => {
            // **The claim goes back.** An error here is not an answer from the
            // person, so holding the claim would silence every later session
            // in this process behind a failure that may not repeat - and
            // Settings would sit on "not asked yet / the next session asks",
            // a promise nothing would keep.
            log::warn!("macOS notification authorization failed: {err}");
            ASKED.store(false, Ordering::SeqCst);
        }
    }
    // Whatever they said, the answer is now a fact the interface has to state.
    refresh();
}

/// Take the one claim on asking the person, returning whether this caller got
/// it.
///
/// A named function over a one-line `swap` because it is the whole of a
/// concurrency rule and the only part of it a test can hold: `swap` is what
/// makes "look" and "take" one operation, and a `load` followed by a `store`
/// here would let three restored surfaces all see it free.
fn claim_the_question(claim: &AtomicBool) -> bool {
    !claim.swap(true, Ordering::SeqCst)
}

/// Post one notification, having first asked macOS whether it will arrive.
///
/// The check is not belt-and-braces: `send` answers `ok` under `Denied`, so
/// without it this function would report a delivery that never happened, which
/// is precisely the failure the whole track was opened to fix.
///
/// **What the check cannot buy**, stated so nobody later reads it as more than
/// it is: the settings are read and then the notification is sent, and the two
/// are not one operation. A refusal made in System Settings in between - or a
/// Focus mode taking effect at delivery - lands after the answer and before the
/// send, and `send` will still say `ok`. The window is microseconds against a
/// human action, and closing it would need a delivery receipt macOS does not
/// offer synchronously; what it rules out is the standing, reproducible case
/// where the channel was already shut before the app ever asked.
///
/// The urgency the caller carries has no home here, and saying so is better
/// than pretending. macOS's two levers for it - `timeSensitive` and `critical`
/// interruption levels - are both gated behind entitlements that an ad-hoc
/// signature does not carry, and ad-hoc is the signature this track measured
/// and chose to stand on. A level we cannot claim would be dropped by the OS
/// without a word.
pub(crate) fn deliver(summary: &str, body: &str) -> Result<(), String> {
    let permission = refresh();
    if !permission.channel_is_open() {
        let (word, _) = permission.stated();
        return Err(format!("macOS notifications are {word}"));
    }
    blocking::send(Notification::new().title(summary).message(body))
        .map(|_id| ())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim on the question is taken once, and the loser of the race is
    /// the caller that must not raise a second system alert.
    ///
    /// It drives `claim_the_question` - the function production calls - over a
    /// fresh flag rather than a hand-rolled `swap` beside it. The earlier
    /// version of this test swapped a local `AtomicBool` and so proved only
    /// that the standard library works, which it was not the one in doubt.
    #[test]
    fn only_one_surface_of_a_restored_container_gets_to_ask() {
        let asked = AtomicBool::new(false);
        assert!(claim_the_question(&asked), "the first one asks");
        assert!(!claim_the_question(&asked), "the second one does not");
        assert!(!claim_the_question(&asked), "nor the third");

        // And a failed ask hands the claim back, which is the arm that decides
        // whether one transient error silences the rest of the process.
        asked.store(false, Ordering::SeqCst);
        assert!(claim_the_question(&asked), "a later session may try again");
    }

    /// The cache is a byte, and a byte that does not round-trip is a state the
    /// interface would state as some other state.
    #[test]
    fn every_permission_survives_the_cache() {
        for permission in MacNotificationPermission::ALL {
            assert_eq!(
                MacNotificationPermission::from_byte(permission.as_byte()),
                permission
            );
        }
        // Two states sharing a byte would be the same defect in reverse: one of
        // them would be stated as the other.
        let mut bytes: Vec<u8> = MacNotificationPermission::ALL
            .iter()
            .map(|permission| permission.as_byte())
            .collect();
        bytes.sort_unstable();
        let count = bytes.len();
        bytes.dedup();
        assert_eq!(bytes.len(), count, "every state has its own byte");

        // A byte nobody wrote reads as "we have not looked", not as an answer.
        assert_eq!(
            MacNotificationPermission::from_byte(200),
            MacNotificationPermission::Unchecked
        );
    }

    /// **Three answers, not two**, and the difference is what stops the
    /// interface claiming coverage it has no evidence for.
    #[test]
    fn ignorance_is_its_own_answer_and_never_a_yes() {
        // Known to arrive.
        assert_eq!(
            MacNotificationPermission::Allowed.will_arrive(),
            Some(true),
            "a banner reaches the screen"
        );
        assert_eq!(
            MacNotificationPermission::AllowedQuietly.will_arrive(),
            Some(true),
            "Notification Center is somewhere a person can go and look"
        );
        // Known not to arrive - including the state that reports `Authorized`
        // and puts the notification in neither of the two places it could go.
        for shut in [
            MacNotificationPermission::NotAsked,
            MacNotificationPermission::AllowedNowhere,
            MacNotificationPermission::Denied,
            MacNotificationPermission::NoBundle,
        ] {
            assert_eq!(shut.will_arrive(), Some(false), "{shut:?} arrives nowhere");
        }
        // Not known either way, and the interface must say so rather than pick.
        for unread in [
            MacNotificationPermission::Unchecked,
            MacNotificationPermission::Unknown,
        ] {
            assert_eq!(unread.will_arrive(), None, "{unread:?} is not an answer");
        }
    }

    /// The sender guesses in the cheap direction where the interface refuses to
    /// guess at all - the one place these two questions are allowed to differ.
    #[test]
    fn the_channel_is_closed_only_where_nothing_can_arrive() {
        assert!(MacNotificationPermission::Allowed.channel_is_open());
        assert!(MacNotificationPermission::AllowedQuietly.channel_is_open());
        assert!(!MacNotificationPermission::NotAsked.channel_is_open());
        assert!(!MacNotificationPermission::Denied.channel_is_open());
        assert!(!MacNotificationPermission::NoBundle.channel_is_open());
        assert!(!MacNotificationPermission::AllowedNowhere.channel_is_open());

        // A missed notification is the expensive failure and a wasted send is
        // the cheap one, so the unread states send anyway - while `will_arrive`
        // still refuses to tell anybody they are covered.
        for unread in [
            MacNotificationPermission::Unchecked,
            MacNotificationPermission::Unknown,
        ] {
            assert!(unread.channel_is_open(), "{unread:?} still sends");
            assert_eq!(
                unread.will_arrive(),
                None,
                "{unread:?} still claims nothing"
            );
        }
    }

    /// Every state Settings can find itself in has a word, and every state a
    /// person cannot act on from inside this app says where they can.
    #[test]
    fn every_state_has_a_word_and_the_dead_ends_say_the_way_out() {
        for permission in MacNotificationPermission::ALL {
            let (word, _) = permission.stated();
            assert!(!word.is_empty(), "{permission:?} has a word");
        }
        // Every state that arrives nowhere has to say something about why, or
        // the row is a bare word the reader can do nothing with.
        for shut in MacNotificationPermission::ALL
            .into_iter()
            .filter(|permission| permission.will_arrive() == Some(false))
        {
            assert!(
                shut.stated().1.is_some(),
                "{shut:?} explains itself rather than just refusing"
            );
        }
        // The one state the app can neither reach nor escape has to name the
        // place that can.
        let (_, denied) = MacNotificationPermission::Denied.stated();
        assert!(
            denied.is_some_and(|note| note.contains("System Settings")),
            "a denial names the only door out of itself"
        );
    }
}
