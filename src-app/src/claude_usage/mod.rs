//! Reading the Claude plan's usage limits.
//!
//! There is no documented API for this. The organisation rate limits in the
//! published docs (ITPM/OTPM) are a different thing entirely, and no CLI flag
//! reports the subscription windows. The numbers come from the undocumented
//! endpoint the `claude` CLI itself lives on:
//!
//! ```text
//! GET https://api.anthropic.com/api/oauth/usage
//! Authorization: Bearer <oauth access token>
//! anthropic-beta: oauth-2025-04-20
//! User-Agent: claude-code/<cli version>
//! ```
//!
//! Because it is undocumented it may change or vanish without warning, so
//! every failure here has to degrade **visibly**: the rail's footer keeps its
//! shape and says what went wrong (see `app::agents_sidebar::limits_footer`).
//! A footer that disappeared would read as "no limits", which is a stronger
//! and wronger claim than "not read".
//!
//! Everything in this module is blocking I/O - a subprocess, a file read, an
//! HTTP call - and must run inside `smol::unblock`, never on the GPUI thread.

pub(crate) mod credentials;
pub(crate) use std::time::Duration;

/// The endpoint. Not configurable: an override would be a way to point the
/// app's OAuth token at a host of somebody else's choosing.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// The beta the endpoint is gated behind.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// What we claim to be. The endpoint answers `claude-code`, so that is what we
/// say - with the CLI's real version when we can read it, and a plausible one
/// when we cannot.
const FALLBACK_CLI_VERSION: &str = "2.1.0";

/// Upper bound on the request. Same reasoning as the update checker's: ureq 3
/// has no default timeout, and a half-open connection would otherwise pin a
/// background worker until the app is killed.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Network failures get one retry, after this pause. A transient DNS blip or a
/// laptop a second out of sleep is worth one more try; anything past that is
/// the caller's next tick, half an hour away.
const RETRY_PAUSE: Duration = Duration::from_secs(3);

/// Bound on the response we will read. The real one is a few kilobytes.
const BODY_CAP: u64 = 256 * 1024;

/// Deadline and cap for `claude --version`.
const VERSION_DEADLINE: Duration = Duration::from_secs(5);
const VERSION_STDOUT_CAP: u64 = 4 * 1024;

/// One usage window: how much of it is gone, and when it starts over.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UsageWindow {
    /// Per cent of the window consumed, `0.0..=100.0`.
    pub(crate) utilization: f32,
    /// Unix seconds at which the window resets, when the server said.
    pub(crate) resets_at: Option<i64>,
}

/// One reading of the account's limits.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UsageSnapshot {
    /// The rolling five-hour session window.
    pub(crate) five_hour: Option<UsageWindow>,
    /// The rolling seven-day window.
    pub(crate) seven_day: Option<UsageWindow>,
}

/// Why a reading failed. Each variant owns the short phrase the footer prints,
/// which is why the enum is small: the line has about thirty characters at the
/// rail's default width, shared with "· last read HH:MM".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LimitsError {
    /// No credentials anywhere the CLI would have put them.
    NotSignedIn,
    /// There were credentials and the server rejected them (401/403).
    ExpiredLogin,
    /// 429. Not retried - the point of a rate limit is to stop asking.
    RateLimited,
    /// DNS, TCP, TLS, timeout.
    Network,
    /// A reply we could not read: the endpoint moved, changed shape, or an
    /// interception proxy answered instead.
    BadReply,
}

impl LimitsError {
    /// The footer's word for it. Kept to thirteen characters or fewer so the
    /// whole line - reason, separator, "last read HH:MM" - fits the rail.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            LimitsError::NotSignedIn => "not signed in",
            LimitsError::ExpiredLogin => "expired login",
            LimitsError::RateLimited => "rate limited",
            LimitsError::Network => "no network",
            LimitsError::BadReply => "bad reply",
        }
    }
}

impl From<credentials::TokenError> for LimitsError {
    fn from(err: credentials::TokenError) -> Self {
        match err {
            // "There is a file and I cannot read it" is not a login state we
            // can name, and the user's move is the same either way: sign in
            // again. Saying "not signed in" is the honest floor.
            credentials::TokenError::NotSignedIn => LimitsError::NotSignedIn,
            credentials::TokenError::Unreadable(why) => {
                log::warn!("claude usage: credentials unusable ({why})");
                LimitsError::NotSignedIn
            }
        }
    }
}

/// One read, off the render thread.
///
/// Historical note: this used to answer with a seven-day series alongside the
/// snapshot, sampled by us because the endpoint carries none. We removed
/// the chart that consumed it - seven bars read as a record of the week, and
/// what we could source was a record of when the app was watching.
pub(crate) fn poll() -> Result<UsageSnapshot, LimitsError> {
    read_usage()
}

/// Read the limits once, with a single retry for network-shaped failures.
fn read_usage() -> Result<UsageSnapshot, LimitsError> {
    let token = credentials::access_token()?;
    let version = cli_version();

    match request(&token, &version) {
        Err(LimitsError::Network) => {
            std::thread::sleep(RETRY_PAUSE);
            request(&token, &version)
        }
        other => other,
    }
}

/// One HTTP call.
fn request(token: &str, cli_version: &str) -> Result<UsageSnapshot, LimitsError> {
    let response = ureq::get(USAGE_URL)
        .config()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", OAUTH_BETA)
        .header("User-Agent", &format!("claude-code/{cli_version}"))
        .header("Accept", "application/json")
        .call();

    let mut response = match response {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(401 | 403)) => return Err(LimitsError::ExpiredLogin),
        Err(ureq::Error::StatusCode(429)) => return Err(LimitsError::RateLimited),
        Err(ureq::Error::StatusCode(code)) => {
            log::warn!("claude usage: endpoint answered HTTP {code}");
            return Err(LimitsError::BadReply);
        }
        Err(err) => {
            // The message can carry the URL but never a header, so it cannot
            // carry the token.
            log::debug!("claude usage: request failed: {err}");
            return Err(LimitsError::Network);
        }
    };

    let body = match response
        .body_mut()
        .with_config()
        .limit(BODY_CAP)
        .read_to_string()
    {
        Ok(body) => body,
        Err(err) => {
            log::warn!("claude usage: could not read the reply: {err}");
            return Err(LimitsError::BadReply);
        }
    };

    parse_usage(&body)
}

/// Turn the endpoint's JSON into the two windows the footer draws.
///
/// Deliberately tolerant. The reply carries considerably more than any UI
/// shows, including code names for unreleased things (`tangelo`,
/// `nimbus_quill`, `iguana_necktie`, `cinder_cove`, `amber_ladder`), and the
/// per-model buckets (`seven_day_opus`, `seven_day_sonnet`) were `null` on
/// every sample we have. Unknown keys are ignored rather than fatal - an
/// endpoint that grows a field must not blank the footer.
pub(crate) fn parse_usage(body: &str) -> Result<UsageSnapshot, LimitsError> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        log::warn!("claude usage: reply is not JSON ({} bytes)", body.len());
        return Err(LimitsError::BadReply);
    };

    let snapshot = UsageSnapshot {
        five_hour: parse_window(value.get("five_hour")),
        seven_day: parse_window(value.get("seven_day")),
    };

    if snapshot.five_hour.is_none() && snapshot.seven_day.is_none() {
        if let Some(object) = value.as_object() {
            // Names only. The values are the user's usage, and one of the
            // reasons to look at this line is an endpoint that started
            // answering with something else entirely.
            let keys: Vec<&str> = object.keys().map(String::as_str).take(24).collect();
            log::warn!("claude usage: no windows in the reply; keys were {keys:?}");
        } else {
            log::warn!("claude usage: reply is JSON but not an object");
        }
        return Err(LimitsError::BadReply);
    }

    log::debug!(
        "claude usage: five_hour={:?} seven_day={:?}",
        snapshot.five_hour,
        snapshot.seven_day
    );
    Ok(snapshot)
}

/// One window object, if it is there and has a utilisation in it.
fn parse_window(value: Option<&serde_json::Value>) -> Option<UsageWindow> {
    let object = value?;
    if object.is_null() {
        return None;
    }
    let utilization = object
        .get("utilization")
        .and_then(serde_json::Value::as_f64)?;
    Some(UsageWindow {
        // The server states a per cent. Clamped because a meter that overflows
        // its track is a rendering bug, and 100 already means "reached".
        utilization: utilization.clamp(0.0, 100.0) as f32,
        resets_at: object.get("resets_at").and_then(parse_reset_time),
    })
}

/// `resets_at` as Unix seconds, from either a number or an RFC 3339 string.
///
/// Both forms are accepted because we have no contract to hold the endpoint
/// to: a number that is plainly milliseconds is folded down rather than read
/// as a date thirty thousand years out.
fn parse_reset_time(value: &serde_json::Value) -> Option<i64> {
    if let Some(seconds) = value.as_i64() {
        return Some(if seconds > 100_000_000_000 {
            seconds / 1000
        } else {
            seconds
        });
    }
    if let Some(text) = value.as_str() {
        return chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|dt| dt.timestamp());
    }
    None
}

/// The CLI's version, for the User-Agent. Probed once per process.
///
/// A wrong version here is not fatal - the endpoint answered every version we
/// tried - so a missing `claude` on PATH falls back to a plausible constant
/// rather than failing the read.
fn cli_version() -> String {
    use std::sync::OnceLock;
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(probe_cli_version).clone()
}

fn probe_cli_version() -> String {
    let mut cmd = std::process::Command::new("claude");
    cmd.arg("--version");
    let Ok(output) = splitlane_process::run_with_timeout(cmd, VERSION_DEADLINE, VERSION_STDOUT_CAP)
    else {
        return FALLBACK_CLI_VERSION.to_string();
    };
    if !output.status.success() {
        return FALLBACK_CLI_VERSION.to_string();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    extract_version(&text).unwrap_or_else(|| FALLBACK_CLI_VERSION.to_string())
}

/// Pull `2.1.237` out of `claude --version`'s `2.1.237 (Claude Code)`.
fn extract_version(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    let version = token.trim().trim_start_matches('v');
    let looks_like_a_version = version.split('.').count() >= 2
        && version
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c.is_ascii_alphabetic());
    looks_like_a_version.then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape we have actually seen, trimmed to what the footer reads.
    const SAMPLE: &str = r#"{
        "five_hour":  {"utilization": 63.0, "resets_at": "2026-08-21T14:00:00Z"},
        "seven_day":  {"utilization": 21.5, "resets_at": 1787000000},
        "seven_day_opus": null,
        "seven_day_sonnet": null,
        "tangelo": {"whatever": true},
        "extra_usage": {"used_credits": 0, "monthly_limit": 0, "decimal_places": 2}
    }"#;

    #[test]
    fn both_windows_come_out_of_a_real_reply() {
        let snapshot = parse_usage(SAMPLE).unwrap();
        let five = snapshot.five_hour.unwrap();
        assert!((five.utilization - 63.0).abs() < f32::EPSILON);
        assert_eq!(five.resets_at, Some(1787320800));
        let seven = snapshot.seven_day.unwrap();
        assert!((seven.utilization - 21.5).abs() < f32::EPSILON);
        assert_eq!(seven.resets_at, Some(1787000000));
    }

    /// Unreleased code names and null per-model buckets must not break a read.
    #[test]
    fn unknown_keys_and_null_buckets_are_ignored() {
        let body =
            r#"{"five_hour":{"utilization":1},"iguana_necktie":{"a":1},"seven_day_opus":null}"#;
        let snapshot = parse_usage(body).unwrap();
        assert!(snapshot.five_hour.is_some());
        assert!(snapshot.seven_day.is_none());
    }

    /// A reply with neither window is the endpoint having moved, not a zero
    /// reading - showing 0% there would be a lie.
    #[test]
    fn a_reply_without_windows_is_a_bad_reply() {
        assert_eq!(parse_usage(r#"{"ok":true}"#), Err(LimitsError::BadReply));
        assert_eq!(parse_usage("<html>nope</html>"), Err(LimitsError::BadReply));
        assert_eq!(parse_usage("[]"), Err(LimitsError::BadReply));
    }

    #[test]
    fn utilization_is_clamped_to_the_meter() {
        let over = parse_usage(r#"{"five_hour":{"utilization":140}}"#).unwrap();
        assert!((over.five_hour.unwrap().utilization - 100.0).abs() < f32::EPSILON);
        let under = parse_usage(r#"{"five_hour":{"utilization":-3}}"#).unwrap();
        assert!(under.five_hour.unwrap().utilization.abs() < f32::EPSILON);
    }

    #[test]
    fn reset_times_are_read_from_seconds_millis_or_rfc3339() {
        let secs = serde_json::json!(1787000000i64);
        assert_eq!(parse_reset_time(&secs), Some(1787000000));
        let millis = serde_json::json!(1787000000000i64);
        assert_eq!(parse_reset_time(&millis), Some(1787000000));
        let text = serde_json::json!("2026-08-21T14:00:00+02:00");
        assert_eq!(parse_reset_time(&text), Some(1787313600));
        assert_eq!(parse_reset_time(&serde_json::json!("soon")), None);
        assert_eq!(parse_reset_time(&serde_json::Value::Null), None);
    }

    #[test]
    fn a_window_without_a_utilization_is_no_window() {
        let body = r#"{"five_hour":{"resets_at":1},"seven_day":{"utilization":5}}"#;
        assert!(parse_usage(body).unwrap().five_hour.is_none());
    }

    #[test]
    fn the_cli_version_comes_off_the_front_of_the_line() {
        assert_eq!(
            extract_version("2.1.237 (Claude Code)").as_deref(),
            Some("2.1.237")
        );
        assert_eq!(extract_version("v2.1.237\n").as_deref(), Some("2.1.237"));
        assert_eq!(extract_version("1.0.0-rc.1").as_deref(), Some("1.0.0-rc.1"));
        assert_eq!(extract_version("command not found"), None);
        assert_eq!(extract_version(""), None);
    }

    /// Every reason fits the line the footer has for it.
    #[test]
    fn every_reason_is_short_enough_for_the_rail() {
        for err in [
            LimitsError::NotSignedIn,
            LimitsError::ExpiredLogin,
            LimitsError::RateLimited,
            LimitsError::Network,
            LimitsError::BadReply,
        ] {
            assert!(err.reason().len() <= 13, "{:?} is too long", err);
        }
    }
}
