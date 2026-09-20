//! Desktop notification routing for agent lifecycle events.
//!
//! This module owns both sides of the notification gate:
//! - the process-wide focus flag updated by the GPUI app;
//! - the one firing path used by `ai.*` handlers.
//!
//! That path used to be described here as "a single cross-platform
//! `notify-rust` firing path", which was true of the code and false about the
//! world: `notify-rust`'s macOS arm has never delivered anything on a current
//! macOS, so the path was cross-platform in shape and two-platform in effect.
//! It is now two arms and says so - see [`show_desktop_notification`] and
//! `crate::agents::mac_notifications`.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::BackgroundExecutor;
use splitlane_config::schema::{AgentPanelConfig, NotifyLevel, SplitlaneConfig};

use crate::agent_launcher::TerminalAgent;
#[cfg(target_os = "windows")]
use crate::windows_app_identity::SPLITLANE_WINDOWS_AUMID;

const NOTIFICATION_DETAIL_CAP_CHARS: usize = 512;

#[cfg(target_os = "windows")]
const SPLITLANE_WINDOWS_NOTIFICATION_ICON_ASSET: &str = "icons/splitlane.png";
#[cfg(target_os = "windows")]
const SPLITLANE_WINDOWS_NOTIFICATION_ICON_FILE: &str = "splitlane-notification.png";

/// Window-active gate updated by `cx.observe_window_activation`.
/// `true` while the OS reports the Splitlane window as the focused one.
///
/// **This is one half of "can the person see it", not the whole of it** - see
/// [`should_fire_desktop_notification`].
static WINDOW_ACTIVE: AtomicBool = AtomicBool::new(true);

/// Update the window-active flag. Called from
/// `cx.observe_window_activation` and from the initial activation
/// tick that GPUI fires when the observer registers.
pub fn set_window_active(active: bool) {
    WINDOW_ACTIVE.store(active, Ordering::Relaxed);
}

/// Is the Splitlane window currently the focused surface?
pub fn window_active() -> bool {
    WINDOW_ACTIVE.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopNotificationUrgency {
    Normal,
    Critical,
}

/// Which rung of the ladder a notification needs the reader to be on.
///
/// The classes exist **before** the setting - they stand behind the five status
/// words and behind the Activity popover - which is what makes a ladder over
/// them a choice among distinctions the reader has already learnt rather than a
/// new vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NotificationClass {
    /// Something broke: a session that fell over, one that has gone silent, and
    /// the plan window that will run out before it comes back.
    ///
    /// The forecast sits here rather than with the finishes because of what it
    /// costs to miss: it is the only class in this app whose price for going
    /// unheard is the rest of the window, and it fires once per window, so the
    /// quietest rung can carry it without becoming noise.
    Broke,
    /// An agent is standing on an answer.
    Waiting,
    /// A run has finished.
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DesktopNotification {
    summary: String,
    body: String,
    urgency: DesktopNotificationUrgency,
    class: NotificationClass,
}

impl DesktopNotification {
    pub(crate) fn turn_finished(
        agent: TerminalAgent,
        subject: &str,
        session_summary: Option<&str>,
    ) -> Self {
        Self {
            summary: format!("{} finished", agent.display_name()),
            body: notification_context_body(subject, session_summary),
            urgency: DesktopNotificationUrgency::Normal,
            class: NotificationClass::Finished,
        }
    }

    pub(crate) fn needs_input(agent: TerminalAgent, subject: &str, message: Option<&str>) -> Self {
        Self {
            summary: format!("{} needs input", agent.display_name()),
            body: attention_notification_body(subject, message),
            urgency: DesktopNotificationUrgency::Critical,
            class: NotificationClass::Waiting,
        }
    }

    pub(crate) fn agent_exited(agent: TerminalAgent, subject: &str, exit_code: i32) -> Self {
        Self {
            summary: format!("{} exited unexpectedly", agent.display_name()),
            body: agent_exit_notification_body(subject, exit_code),
            urgency: DesktopNotificationUrgency::Critical,
            class: NotificationClass::Broke,
        }
    }

    /// The plan's window will be spent before it resets, at the rate two
    /// readings apart have measured.
    ///
    /// **The event is the projection, not a threshold.** Crossing 85% is a
    /// state, and the rail's footer already carries states; what changes what
    /// a person does now is learning they will hit the wall before the window
    /// comes back. `lines` is one entry per window in that condition, so two
    /// windows going at once arrive as one notification rather than two
    /// competing for the same attention.
    ///
    /// Normal rather than critical: nothing is broken and nothing is waiting on
    /// an answer. It is a fact that changes a plan, and the three critical
    /// notifications beside it all mean a session has stopped.
    pub(crate) fn limit_forecast(summary: String, body: String) -> Self {
        Self {
            summary,
            body,
            urgency: DesktopNotificationUrgency::Normal,
            class: NotificationClass::Broke,
        }
    }

    pub(crate) fn stalled(agent: TerminalAgent, subject: &str, silent_secs: u64) -> Self {
        Self {
            summary: format!("{} may be stuck", agent.display_name()),
            body: stalled_notification_body(subject, silent_secs),
            urgency: DesktopNotificationUrgency::Critical,
            class: NotificationClass::Broke,
        }
    }
}

/// Fire a best-effort desktop notification without blocking the GPUI thread.
///
/// **Answers whether it said anything**, because a caller that keeps a "said
/// this already" mark has to set it on the saying and not on the asking. The
/// two gates below turn most calls into silence - a surface the reader is
/// already looking at, a class below the chosen rung - and a caller that marks
/// regardless spends its one chance on a delivery that never happened. That is
/// exactly what the limits forecast did (see `announce_limit_forecast`): a
/// window whose projection first turned bad while the window was in front of
/// the reader was never announced again for that whole epoch.
///
/// `false` is "nothing was sent"; `true` is "the notification was handed to the
/// platform", which is as far as any caller can know - delivery past that point
/// belongs to the desktop and is best-effort by design.
pub(crate) fn fire_desktop_notification(
    notification: DesktopNotification,
    config: &SplitlaneConfig,
    surface_is_seen: bool,
    executor: BackgroundExecutor,
) -> bool {
    let level = config.agent_panel.as_ref().map_or_else(
        NotifyLevel::default,
        AgentPanelConfig::resolved_notify_level,
    );
    if !should_fire_desktop_notification(level, notification.class, surface_is_seen) {
        return false;
    }

    executor
        .spawn(async move {
            let _ = smol::unblock(move || show_desktop_notification(notification)).await;
        })
        .detach();
    true
}

/// Two questions, and both have to answer yes.
///
/// **Can they see it?** is the older one and unchanged: a session whose pane is
/// on screen in the active container of an active window has already told them,
/// and a notification about it would be the app talking over itself.
///
/// **Do they want to hear about this kind of thing?** is the ladder, and it is
/// an *ordering* rather than a set of switches - each rung admits its own class
/// and every class below it, which is what makes the rungs readable as one
/// sentence continued ("only when something breaks" / "when an agent is waiting
/// for me" / "and when work has finished") and what makes a breakage
/// unsilenceable.
pub(crate) fn should_fire_desktop_notification(
    level: NotifyLevel,
    class: NotificationClass,
    surface_is_seen: bool,
) -> bool {
    let wanted = match class {
        NotificationClass::Broke => true,
        NotificationClass::Waiting => level >= NotifyLevel::Waiting,
        NotificationClass::Finished => level >= NotifyLevel::Finished,
    };
    wanted && !surface_is_seen
}

/// Which rung covers a class, for the Settings rows that state it.
pub(crate) fn class_is_covered(level: NotifyLevel, class: NotificationClass) -> bool {
    should_fire_desktop_notification(level, class, false)
}

/// The shortest turn worth telling somebody about.
///
/// Without a floor, an agent answering a one-line question in four seconds
/// sends a toast, and with four agents running that is what makes a person
/// turn the whole feature off. The number is `undistract-me`'s - the tool that
/// established this pattern on Linux ("notify when a command that took longer
/// than N seconds finishes and the terminal is not focused") - and ten seconds
/// is long enough that nobody sat waiting for it and short enough that no real
/// piece of work slips under.
///
/// **A constant and not a setting.** Settings -> Notifications opens with "One
/// switch", and a second knob would contradict it for a number nobody has an
/// opinion about until they have been annoyed by the wrong one. If it ever
/// turns out to be wrong it is a number to change, not a question to ask.
///
/// It bounds **completion only**. "Needs input", "crashed" and "stalled" are
/// claims on a person's attention that a short turn does not make less true -
/// an agent that dies in two seconds is exactly the one worth hearing about.
pub(crate) const MIN_TURN_FOR_NOTIFICATION: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Whether a finished turn of this length is worth a notification.
///
/// `None` is "we do not know how long it ran", and that answers **yes**: the
/// unknown cases are a hook frame that arrived without a start (an agent
/// launched before this build learned to stamp one) and a restored session, and
/// silence there would be a completion nobody ever hears about. Over-telling in
/// a case the app cannot measure is the better error - the floor exists to stop
/// noise from short turns, not to stop notifications it cannot classify.
pub(crate) fn turn_was_long_enough(ran_for: Option<std::time::Duration>) -> bool {
    ran_for.is_none_or(|ran| ran >= MIN_TURN_FOR_NOTIFICATION)
}

/// Bound + sanitize an agent question before it is stored on the session
/// and mirrored to notifications.
pub(crate) fn sanitize_notification_message(raw: &str) -> String {
    crate::markdown::strip_bidi_zero_width(raw.chars().take(512).collect())
}

fn notification_detail(raw: &str) -> Option<String> {
    let clean: String = crate::markdown::strip_bidi_zero_width(
        raw.chars().take(NOTIFICATION_DETAIL_CAP_CHARS).collect(),
    )
    .trim()
    .to_string();
    (!clean.is_empty()).then_some(clean)
}

/// The body of a notification about one session: **its name first, then what
/// it said.**
///
/// The two are **joined, not chosen between**, and that is the whole content of
/// this function. It used to be `summary.or_else(|| name)`, which dropped the
/// name in exactly the case where the notification was worth reading - an agent
/// with something specific to say - so two sessions in one project produced two
/// pings reading `Claude Code needs input` over two bodies that named neither.
/// The question a notification has to answer at four agents is **which one**,
/// and `subject` is the answer: the caller passes the surface's own name on the
/// agent arm and the project's on the workspace arm.
///
/// Found twice in use a week apart - "it tells you that something
/// finished, **not which thing** or what it decided; six identical bells is just
/// six reasons to go look", and "**if the ping ever carried the session name**, a
/// different glow colour per session would cover that". The attention popover
/// beside it had the right answer all along (`app/attention_queue.rs` carries
/// the session's own name over `project · branch`); the desktop notification was
/// the one surface that answered worse.
///
/// **Order is also the truncation rule.** Every notification service cuts the
/// tail, so leading with the name means an over-long message loses itself rather
/// than the identity - which is why the two parts keep their own caps instead of
/// sharing a joint one. `agent_exit_notification_body` and
/// `stalled_notification_body` already had this shape; the other two now agree
/// with them.
fn notification_context_body(subject: &str, session_summary: Option<&str>) -> String {
    match (
        notification_detail(subject),
        session_summary.and_then(notification_detail),
    ) {
        (Some(name), Some(detail)) => format!("{name}: {detail}"),
        (Some(name), None) => name,
        (None, Some(detail)) => detail,
        (None, None) => "Splitlane".to_string(),
    }
}

pub(crate) fn attention_notification_body(subject: &str, message: Option<&str>) -> String {
    notification_context_body(subject, message)
}

pub(crate) fn agent_exit_notification_body(subject: &str, exit_code: i32) -> String {
    format!(
        "{}: exited with code {exit_code}",
        notification_context_body(subject, None)
    )
}

pub(crate) fn stalled_notification_body(subject: &str, silent_secs: u64) -> String {
    format!(
        "{}: no activity for {silent_secs} s",
        notification_context_body(subject, None)
    )
}

/// Hand one notification to the platform.
///
/// **Two backends, and the split is the track's whole finding.** Linux and
/// Windows go through `notify-rust`, which is correct on both. macOS goes
/// through [`crate::agents::mac_notifications`], because `notify-rust`'s macOS
/// arm is built on `NSUserNotificationCenter` - deprecated in 10.14 and
/// silently ignored since, which is why this app's notifications had never once
/// arrived on that platform while every call site here answered `Ok(())`.
///
/// The `Result` is what the caller may believe, and on macOS it is now worth
/// believing: the macOS arm asks the system whether a notification will arrive
/// before it claims to have sent one.
#[cfg(not(target_os = "macos"))]
fn show_desktop_notification(notification: DesktopNotification) -> Result<(), String> {
    let mut builder = notify_rust::Notification::new();
    builder
        .summary(&notification.summary)
        .body(&notification.body)
        .appname("Splitlane")
        .icon("splitlane")
        .timeout(std::time::Duration::from_secs(8));

    builder.urgency(notification_urgency_for_platform(notification.urgency));

    #[cfg(unix)]
    builder.hint(notify_rust::Hint::DesktopEntry("splitlane".to_string()));

    #[cfg(target_os = "windows")]
    {
        let _ = crate::windows_app_identity::ensure_process_app_user_model_id();
        let _ = ensure_windows_app_user_model_id_registered();
        builder.app_id(SPLITLANE_WINDOWS_AUMID);
    }

    builder.show().map(|_| ()).map_err(|err| err.to_string())
}

/// See the note on the other arm. The urgency is dropped here rather than
/// translated: both of macOS's levers for it are entitlement-gated, and an
/// ad-hoc signature - the one this track measured and stands on - carries no
/// entitlements.
#[cfg(target_os = "macos")]
fn show_desktop_notification(notification: DesktopNotification) -> Result<(), String> {
    crate::agents::mac_notifications::deliver(&notification.summary, &notification.body)
}

#[cfg(not(target_os = "macos"))]
fn notification_urgency_for_platform(urgency: DesktopNotificationUrgency) -> notify_rust::Urgency {
    match urgency {
        DesktopNotificationUrgency::Normal => notify_rust::Urgency::Normal,
        DesktopNotificationUrgency::Critical => notify_rust::Urgency::Critical,
    }
}

#[cfg(target_os = "windows")]
fn ensure_windows_app_user_model_id_registered() -> Result<(), String> {
    let key_path = format!(r"SOFTWARE\Classes\AppUserModelId\{SPLITLANE_WINDOWS_AUMID}");
    let key = windows_registry::CURRENT_USER
        .create(&key_path)
        .map_err(|err| format!("create HKCU\\{key_path}: {err}"))?;
    key.set_string("DisplayName", "Splitlane")
        .map_err(|err| format!("set DisplayName: {err}"))?;
    key.set_string("IconBackgroundColor", "0")
        .map_err(|err| format!("set IconBackgroundColor: {err}"))?;
    let icon_path = ensure_windows_notification_icon()?;
    key.set_hstring("IconUri", &icon_path.as_path().into())
        .map_err(|err| format!("set IconUri: {err}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn ensure_windows_notification_icon() -> Result<std::path::PathBuf, String> {
    let data = crate::assets::Assets::get(SPLITLANE_WINDOWS_NOTIFICATION_ICON_ASSET)
        .ok_or_else(|| {
            format!(
                "embedded notification icon {SPLITLANE_WINDOWS_NOTIFICATION_ICON_ASSET} not found"
            )
        })?
        .data;
    let icon_dir = crate::runtime_paths::data_dir()
        .ok_or_else(|| "Splitlane data dir is unavailable for notification icon".to_string())?
        .join("icons");
    std::fs::create_dir_all(&icon_dir)
        .map_err(|err| format!("create notification icon dir {}: {err}", icon_dir.display()))?;

    let icon_path = icon_dir.join(SPLITLANE_WINDOWS_NOTIFICATION_ICON_FILE);
    let needs_write = match std::fs::read(&icon_path) {
        Ok(existing) => existing != data.as_ref(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Err(err) => {
            return Err(format!(
                "read notification icon {}: {err}",
                icon_path.display()
            ));
        }
    };
    if needs_write {
        std::fs::write(&icon_path, data.as_ref())
            .map_err(|err| format!("write notification icon {}: {err}", icon_path.display()))?;
    }
    Ok(icon_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two questions, and both have to answer yes.
    #[test]
    fn the_gate_asks_the_rung_and_then_whether_they_can_already_see_it() {
        // On screen: the dot is right there, so nothing is sent, whatever the
        // rung and whatever the class.
        for class in [
            NotificationClass::Broke,
            NotificationClass::Waiting,
            NotificationClass::Finished,
        ] {
            assert!(!should_fire_desktop_notification(
                NotifyLevel::Finished,
                class,
                true
            ));
        }
        // The default rung: an agent standing on an answer, and a breakage.
        assert!(should_fire_desktop_notification(
            NotifyLevel::Waiting,
            NotificationClass::Waiting,
            false
        ));
        assert!(should_fire_desktop_notification(
            NotifyLevel::Waiting,
            NotificationClass::Broke,
            false
        ));
        // And not a finish, which is the rung above.
        assert!(!should_fire_desktop_notification(
            NotifyLevel::Waiting,
            NotificationClass::Finished,
            false
        ));
        assert!(should_fire_desktop_notification(
            NotifyLevel::Finished,
            NotificationClass::Finished,
            false
        ));
    }

    /// **The floor of the scale is still a working product.** There is no rung
    /// that silences a breakage, which is what replaces a lock: the setting
    /// cannot switch off the thing that would report the setting was a mistake.
    #[test]
    fn the_bottom_rung_still_says_when_something_broke() {
        assert!(should_fire_desktop_notification(
            NotifyLevel::Broke,
            NotificationClass::Broke,
            false
        ));
        assert!(!should_fire_desktop_notification(
            NotifyLevel::Broke,
            NotificationClass::Waiting,
            false
        ));
        assert!(!should_fire_desktop_notification(
            NotifyLevel::Broke,
            NotificationClass::Finished,
            false
        ));
    }

    /// The gate is visibility, not window focus. The old question was
    /// "is Splitlane the focused window", which assumed that being in the app
    /// means seeing the session - true of one pane, false of the four-across-
    /// three-projects case the app exists for.
    #[test]
    fn a_surface_you_cannot_see_notifies_even_from_inside_the_app() {
        // The window is focused and the pane is not on screen: this is the
        // reported case, and it used to be silent.
        assert!(should_fire_desktop_notification(
            NotifyLevel::Waiting,
            NotificationClass::Waiting,
            false
        ));
        assert!(!should_fire_desktop_notification(
            NotifyLevel::Waiting,
            NotificationClass::Waiting,
            true
        ));
    }

    /// The floor bounds completion only, and an unmeasured run is announced.
    #[test]
    fn a_short_run_is_not_worth_saying_and_an_unmeasured_one_is() {
        use std::time::Duration;
        assert!(!turn_was_long_enough(Some(Duration::from_secs(4))));
        assert!(turn_was_long_enough(Some(MIN_TURN_FOR_NOTIFICATION)));
        assert!(turn_was_long_enough(Some(Duration::from_secs(600))));
        // Not knowing is not a reason to stay silent: the unknown cases are a
        // frame with no start stamp and a session restored mid-run, and a
        // completion nobody hears about is the worse failure.
        assert!(turn_was_long_enough(None));
    }

    #[test]
    fn notification_message_is_bounded_and_bidi_stripped() {
        let spoofed = "Allow \u{202E}?fr- mr\u{202C} ?";
        let clean = sanitize_notification_message(spoofed);
        assert!(!clean.contains('\u{202E}'), "RLO stripped");
        assert!(!clean.contains('\u{202C}'), "PDF stripped");
        assert!(clean.contains("Allow"), "visible text kept: {clean}");

        let long = "é".repeat(600);
        assert_eq!(
            sanitize_notification_message(&long).chars().count(),
            512,
            "char-bounded, multibyte-safe"
        );
    }

    #[test]
    fn notification_bodies_are_specific_and_non_empty() {
        assert_eq!(
            attention_notification_body("backend", Some("Allow `cargo test`?")),
            "backend: Allow `cargo test`?"
        );
        assert_eq!(attention_notification_body("backend", None), "backend");
        assert_eq!(
            attention_notification_body("backend", Some("   ")),
            "backend"
        );
        assert_eq!(
            agent_exit_notification_body("api", 1),
            "api: exited with code 1"
        );
        assert_eq!(
            stalled_notification_body("api", 300),
            "api: no activity for 300 s"
        );
        assert_eq!(
            notification_context_body("workspace", Some("Finished the release draft")),
            "workspace: Finished the release draft"
        );
        // A session the caller cannot name still says what it said, rather
        // than losing the message to a "Splitlane" placeholder.
        assert_eq!(
            notification_context_body("  ", Some("Approve edit?")),
            "Approve edit?"
        );
        assert_eq!(notification_context_body("  ", None), "Splitlane");
    }

    /// The reason this module joins instead of choosing: with two sessions of
    /// the same agent in one project, the summary is identical by construction
    /// (`"Claude Code needs input"`), so the **body** is the only place the
    /// answer to "which one" can live. It used to be dropped exactly when a
    /// hook supplied a message, which is exactly when the ping was worth
    /// reading.
    #[test]
    fn two_sessions_of_one_agent_do_not_send_the_same_notification() {
        let auth = DesktopNotification::needs_input(
            TerminalAgent::ClaudeCode,
            "auth refactor",
            Some("Approve edit?"),
        );
        let parser = DesktopNotification::needs_input(
            TerminalAgent::ClaudeCode,
            "parser",
            Some("Approve edit?"),
        );

        assert_eq!(auth.summary, parser.summary);
        assert_ne!(auth.body, parser.body);
        assert!(auth.body.starts_with("auth refactor"));
        assert!(parser.body.starts_with("parser"));
    }

    /// Notification services cut the tail, so the name has to be at the head:
    /// an agent that says too much must lose its own message, never its
    /// identity.
    #[test]
    fn an_over_long_message_never_pushes_the_name_out() {
        let flood = "x".repeat(NOTIFICATION_DETAIL_CAP_CHARS * 3);
        let body = attention_notification_body("parser", Some(&flood));
        assert!(body.starts_with("parser: "));
        assert_eq!(
            body.chars().count(),
            "parser: ".chars().count() + NOTIFICATION_DETAIL_CAP_CHARS
        );
    }

    #[test]
    fn desktop_notification_constructors_set_title_body_and_urgency() {
        let finished =
            DesktopNotification::turn_finished(TerminalAgent::Codex, "backend", Some("Tests pass"));
        assert_eq!(finished.summary, "Codex finished");
        assert_eq!(finished.body, "backend: Tests pass");
        assert_eq!(finished.urgency, DesktopNotificationUrgency::Normal);

        let finished_without_summary =
            DesktopNotification::turn_finished(TerminalAgent::Codex, "backend", None);
        assert_eq!(finished_without_summary.summary, "Codex finished");
        assert_eq!(finished_without_summary.body, "backend");

        let attention = DesktopNotification::needs_input(
            TerminalAgent::ClaudeCode,
            "backend",
            Some("Approve edit?"),
        );
        assert_eq!(attention.summary, "Claude Code needs input");
        assert_eq!(attention.body, "backend: Approve edit?");
        assert_eq!(attention.urgency, DesktopNotificationUrgency::Critical);
    }
}
