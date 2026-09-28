//! Free access (`ai_unrestricted`) opens `send`/`key` over the socket, and only
//! a person in the app can open it.
//!
//! `splitlane.json` is hot-reloaded, and any agent allowed to edit files can
//! edit it. If the key alone opened the gate, an agent could grant itself write
//! access to every other pane - the opposite of the opt-in the setting
//! promises. So the key is read as a *request*: it opens the gate only once the
//! Settings switch has been turned on in this run of the app.
//!
//! The confirmation lives in this process and nowhere else. A record on disk -
//! a second file, a hash, a timestamp - is written with the same rights as
//! `splitlane.json` itself, so an agent able to forge one could forge the
//! other; it would add a step, not a boundary. The cost is that a person who
//! keeps free access on confirms it once per launch, and is told so.
//!
//! Turning it off needs no confirmation: a file that says `false` closes the
//! gate at once and drops the confirmation, so a later `true` asks again.

use gpui::Context;

use crate::SplitlaneApp;

/// Whether free access has been confirmed in the app during this run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FreeAccess {
    confirmed: bool,
    /// Whether the person has been told this run that the file is asking.
    /// Once is enough: an agent that flips the key back and forth must not be
    /// able to raise the notice - and its button to the switch - at will.
    announced: bool,
}

impl FreeAccess {
    /// Whether the write gate is open, given what `splitlane.json` requests.
    pub(crate) fn is_open(self, requested: bool) -> bool {
        requested && self.confirmed
    }

    /// The file asks for free access, and nobody has confirmed it here yet.
    pub(crate) fn awaiting_confirmation(self, requested: bool) -> bool {
        requested && !self.confirmed
    }

    /// Take in a value read from `splitlane.json` (at launch, `was_requested`
    /// is `false`). `false` revokes at once. Returns `true` when the file has
    /// just started asking for access that is not confirmed, which is when the
    /// person should be told why it is still off.
    pub(crate) fn observe_file(&mut self, was_requested: bool, requested: bool) -> bool {
        if !requested {
            self.confirmed = false;
            return false;
        }
        !was_requested && !self.confirmed
    }

    /// Whether to show the "confirm it in Settings" notice now. Answers `true`
    /// at most once per run.
    pub(crate) fn take_announcement(&mut self) -> bool {
        !std::mem::replace(&mut self.announced, true)
    }

    /// The Settings switch: the one place free access is turned on.
    pub(crate) fn set_from_settings(&mut self, on: bool) {
        self.confirmed = on;
    }
}

impl SplitlaneApp {
    /// Whether free access currently opens `send`/`key`.
    pub(crate) fn free_access_open(&self) -> bool {
        self.free_access
            .is_open(self.cached_config.ai_unrestricted_enabled())
    }

    /// Whether `splitlane.json` asks for free access that is not confirmed.
    pub(crate) fn free_access_awaiting_confirmation(&self) -> bool {
        self.free_access
            .awaiting_confirmation(self.cached_config.ai_unrestricted_enabled())
    }

    /// Reconcile a freshly read config with the confirmation, before the new
    /// config replaces `cached_config`.
    pub(crate) fn observe_free_access_file(
        &mut self,
        config: &splitlane_config::schema::SplitlaneConfig,
        cx: &mut Context<Self>,
    ) {
        let was_requested = self.cached_config.ai_unrestricted_enabled();
        let requested = config.ai_unrestricted_enabled();
        if self.free_access.observe_file(was_requested, requested) {
            tracing::warn!(
                "ai_unrestricted turned on in splitlane.json; free access stays off until it is confirmed in Settings -> Agents"
            );
            self.announce_free_access_awaiting(cx);
        }
    }

    /// Tell the person that the file asks for free access and where to
    /// confirm it. Held long: it may fire while the window is still coming up.
    pub(crate) fn announce_free_access_awaiting(&mut self, cx: &mut Context<Self>) {
        if !self.free_access.take_announcement() {
            return;
        }
        self.push_toast(
            "Free access is off until you confirm it in Settings".to_string(),
            vec![crate::ToastAction::ReviewFreeAccess],
            crate::app::constants::TOAST_HOLD_MS * 8,
            cx,
        );
    }

    /// The Settings switch. Confirms (or revokes) in this process, then
    /// writes the key so the file agrees with what the app does.
    pub(crate) fn set_free_access_from_settings(&mut self, on: bool, cx: &mut Context<Self>) {
        self.free_access.set_from_settings(on);
        self.persist_setting(false, "ai_unrestricted", serde_json::Value::Bool(on), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::FreeAccess;

    #[test]
    fn off_by_default() {
        let access = FreeAccess::default();
        assert!(!access.is_open(false));
        assert!(!access.awaiting_confirmation(false));
    }

    #[test]
    fn the_notice_is_shown_once_per_run() {
        let mut access = FreeAccess::default();
        assert!(access.take_announcement());
        // An agent toggling the key off and on again gets no second notice.
        access.observe_file(true, false);
        access.observe_file(false, true);
        assert!(!access.take_announcement());
    }

    #[test]
    fn file_turned_on_does_not_open_the_gate() {
        let mut access = FreeAccess::default();
        // A hot reload flips the key from false to true.
        assert!(access.observe_file(false, true), "the person is told");
        assert!(!access.is_open(true));
        assert!(access.awaiting_confirmation(true));
        // Further reloads that keep it true do not open it either, and do
        // not repeat the notice.
        assert!(!access.observe_file(true, true));
        assert!(!access.is_open(true));
    }

    #[test]
    fn file_true_at_launch_waits_for_confirmation() {
        let mut access = FreeAccess::default();
        assert!(access.observe_file(false, true));
        assert!(!access.is_open(true));
    }

    #[test]
    fn settings_switch_opens_and_closes() {
        let mut access = FreeAccess::default();
        access.set_from_settings(true);
        // The switch writes `true`; the watcher then reads it back.
        assert!(!access.observe_file(true, true));
        assert!(access.is_open(true));
        assert!(!access.awaiting_confirmation(true));
        access.set_from_settings(false);
        assert!(!access.is_open(true));
    }

    #[test]
    fn confirming_a_pending_request_opens_it() {
        let mut access = FreeAccess::default();
        access.observe_file(false, true);
        access.set_from_settings(true);
        assert!(access.is_open(true));
    }

    #[test]
    fn file_turned_off_applies_at_once_and_drops_the_confirmation() {
        let mut access = FreeAccess::default();
        access.set_from_settings(true);
        assert!(access.is_open(true));
        assert!(!access.observe_file(true, false));
        assert!(!access.is_open(false));
        // Turning it back on from the file asks again.
        assert!(access.observe_file(false, true));
        assert!(!access.is_open(true));
    }

    #[test]
    fn the_file_value_still_bounds_a_confirmation() {
        let mut access = FreeAccess::default();
        access.set_from_settings(true);
        assert!(!access.is_open(false));
    }
}
