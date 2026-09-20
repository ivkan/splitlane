//! Where Claude Code keeps its OAuth token, and how to read it without ever
//! reading someone else's.
//!
//! The CLI stores one JSON blob - `{"claudeAiOauth":{"accessToken":…}}` - in
//! the macOS Keychain when secure storage is available and in
//! `<config dir>/.credentials.json` otherwise. Keychain exists on exactly one
//! of our three platforms, so the plain file is the **primary** path on Linux
//! and Windows rather than a fallback.
//!
//! The config dir is `$CLAUDE_CONFIG_DIR` when set and `~/.claude` otherwise,
//! and the override changes the Keychain entry's name too:
//! `Claude Code-credentials-<sha256(dir)[0..8]>`. There is deliberately **no
//! fallback to the unscoped name** when the override is set - a different
//! account's numbers, quietly wrong, are worse than an honest "not signed in".
//! Falling through to `<config dir>/.credentials.json` is a different matter
//! and is allowed: that file is scoped to the same directory, and on macOS a
//! Keychain miss is not proof of a logout (the CLI writes the plain file
//! whenever secure storage is unavailable - headless, SSH, a locked keyring).
//!
//! The token is a live key to the user's account. It is read on every request,
//! held only for the length of one request, and never written to a log, a
//! config, an error message or the UI.

use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Base name of the Keychain entry the CLI writes.
///
/// Deliberately NOT `#[cfg(target_os = "macos")]`, though only the macOS
/// Keychain lookup reads it in production. `keychain_service_name` below is
/// pure string logic and its two unit tests run on every platform, so a `cfg`
/// here deletes the subject of tests that still execute - which is what a
/// first attempt did, and Linux said so immediately.
///
/// The Windows leg reports it as dead for a narrower reason: that job is
/// `cargo check` WITHOUT `--all-targets`, so it compiles no tests and sees
/// only the production reader. An `allow` scoped to the platforms where that
/// is true says exactly that, where a `cfg` would say something false.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Upper bound on the `security` subprocess. Reading one generic password is a
/// local call; a second is already pathological, and the caller is a
/// background poll that must not wedge.
#[cfg(target_os = "macos")]
const KEYCHAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Upper bound on the credential blob. The real one is under a kilobyte.
#[cfg(target_os = "macos")]
const KEYCHAIN_STDOUT_CAP: u64 = 64 * 1024;

/// Why there is no token to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenError {
    /// Nothing to read anywhere the CLI would have written it.
    NotSignedIn,
    /// Something was there and could not be understood. Never carries the
    /// file's contents - the file is the secret.
    Unreadable(&'static str),
}

/// The value of `CLAUDE_CONFIG_DIR`, trimmed, if it is set and non-empty.
///
/// On macOS a GUI launch does not inherit the login shell's exports, which is
/// exactly how a usage counter ends up reading a different login than the one
/// the sessions use. [`crate::login_shell_env`] adopts the variable at startup
/// so this lookup sees what a terminal would.
pub(crate) fn config_dir_override() -> Option<String> {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The directory the CLI keeps its state in.
pub(crate) fn config_dir() -> Option<PathBuf> {
    match config_dir_override() {
        Some(dir) => Some(PathBuf::from(dir)),
        None => dirs::home_dir().map(|home| home.join(".claude")),
    }
}

/// The Keychain entry's name for a given `CLAUDE_CONFIG_DIR` value.
///
/// Note on normalisation: the CLI hashes the NFC form of the path. We hash the
/// bytes as the environment hands them to us, which is the same string for an
/// ASCII path and for anything a user typed or exported in NFC. A path handed
/// over in NFD - possible when it was copied out of a macOS directory listing -
/// would hash differently and read as "not signed in" rather than as somebody
/// else's account, which is the failure direction we want.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn keychain_service_name(config_dir_override: Option<&str>) -> String {
    match config_dir_override {
        Some(dir) => format!("{KEYCHAIN_SERVICE}-{}", scope_digits(dir)),
        None => KEYCHAIN_SERVICE.to_string(),
    }
}

/// The eight hex digits a config-dir override is scoped by. Shared with the
/// week cache so one login's samples cannot land in another's chart.
pub(crate) fn scope_digits(config_dir: &str) -> String {
    short_sha256(config_dir)
}

/// First four bytes of the SHA-256 of `input`, lowercase hex.
fn short_sha256(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Read the OAuth access token, or say why there is none.
pub(crate) fn access_token() -> Result<String, TokenError> {
    // Only the Keychain lookup below consumes it, and that is macOS-only.
    #[cfg(target_os = "macos")]
    let override_dir = config_dir_override();

    #[cfg(target_os = "macos")]
    {
        let service = keychain_service_name(override_dir.as_deref());
        match read_keychain(&service) {
            Ok(Some(token)) => return Ok(token),
            Ok(None) => {
                log::debug!("claude usage: no Keychain entry for {service:?}; trying the file");
            }
            Err(err) => return Err(err),
        }
    }

    let Some(dir) = config_dir() else {
        return Err(TokenError::Unreadable("no home directory"));
    };
    read_credentials_file(&dir.join(".credentials.json"))
}

/// Ask the macOS Keychain for the entry. `Ok(None)` means "no such entry",
/// which is not an error: the CLI may have written the plain file instead.
#[cfg(target_os = "macos")]
fn read_keychain(service: &str) -> Result<Option<String>, TokenError> {
    let mut cmd = std::process::Command::new("security");
    cmd.args(["find-generic-password", "-s", service, "-w"]);
    let output =
        match splitlane_process::run_with_timeout(cmd, KEYCHAIN_DEADLINE, KEYCHAIN_STDOUT_CAP) {
            Ok(out) => out,
            Err(splitlane_process::ProcError::Timeout) => {
                return Err(TokenError::Unreadable("keychain timed out"));
            }
            Err(err) => {
                log::debug!("claude usage: could not run `security`: {err}");
                return Ok(None);
            }
        };
    if !output.status.success() {
        // Exit 44 is "the item cannot be found"; anything else is still not
        // worth distinguishing here, because the plain file is the next stop
        // either way. stderr is deliberately not logged: it echoes the service
        // name and, on some failures, the account.
        return Ok(None);
    }
    let blob = String::from_utf8(output.stdout)
        .map_err(|_| TokenError::Unreadable("keychain entry is not text"))?;
    parse_credentials(&blob).map(Some)
}

/// Read `<config dir>/.credentials.json`.
fn read_credentials_file(path: &std::path::Path) -> Result<String, TokenError> {
    match std::fs::read_to_string(path) {
        Ok(body) => parse_credentials(&body),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(TokenError::NotSignedIn),
        Err(err) => {
            log::debug!(
                "claude usage: cannot read {}: {}",
                path.display(),
                err.kind()
            );
            Err(TokenError::Unreadable("credentials unreadable"))
        }
    }
}

/// Pull `claudeAiOauth.accessToken` out of the credential blob.
///
/// Errors never quote the input: every byte of it is secret.
pub(crate) fn parse_credentials(blob: &str) -> Result<String, TokenError> {
    let value: serde_json::Value =
        serde_json::from_str(blob).map_err(|_| TokenError::Unreadable("credentials malformed"))?;
    match value
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|t| t.as_str())
    {
        Some(token) if !token.trim().is_empty() => Ok(token.trim().to_string()),
        _ => Err(TokenError::NotSignedIn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unscoped_entry_is_used_when_there_is_no_override() {
        assert_eq!(keychain_service_name(None), "Claude Code-credentials");
    }

    /// The scoped name is the base plus eight hex digits of the path's hash,
    /// and two different directories never share one.
    #[test]
    fn an_override_scopes_the_entry_to_the_directory() {
        let a = keychain_service_name(Some("/Users/me/.claude-work"));
        let b = keychain_service_name(Some("/Users/me/.claude-play"));
        assert!(a.starts_with("Claude Code-credentials-"), "{a}");
        assert_eq!(a.len(), "Claude Code-credentials-".len() + 8);
        assert!(a.is_ascii());
        assert_ne!(a, b);
        // Stable across calls - the name is a pure function of the path.
        assert_eq!(a, keychain_service_name(Some("/Users/me/.claude-work")));
    }

    /// The digits are the real SHA-256 prefix, not some other hash.
    #[test]
    fn the_scope_digits_are_the_sha256_prefix() {
        // sha256("abc") = ba7816bf8f01cfea…
        assert_eq!(short_sha256("abc"), "ba7816bf");
    }

    #[test]
    fn the_token_comes_out_of_the_oauth_object() {
        let blob = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-xyz","expiresAt":1}}"#;
        assert_eq!(parse_credentials(blob).unwrap(), "sk-ant-oat01-xyz");
    }

    /// A blob that parses but has no token is a logout, not a corruption -
    /// the CLI writes an empty shell after `/logout`.
    #[test]
    fn an_empty_token_reads_as_signed_out() {
        assert_eq!(
            parse_credentials(r#"{"claudeAiOauth":{"accessToken":""}}"#),
            Err(TokenError::NotSignedIn)
        );
        assert_eq!(parse_credentials("{}"), Err(TokenError::NotSignedIn));
    }

    #[test]
    fn garbage_is_unreadable_rather_than_signed_out() {
        assert!(matches!(
            parse_credentials("not json at all"),
            Err(TokenError::Unreadable(_))
        ));
    }

    /// Whatever goes wrong, the message must never carry the blob.
    #[test]
    fn errors_never_quote_the_secret() {
        let secret = "sk-ant-oat01-THISMUSTNOTLEAK";
        let blob = format!("{{\"claudeAiOauth\":{{\"accessToken\":{secret}");
        let err = parse_credentials(&blob).unwrap_err();
        assert!(!format!("{err:?}").contains("THISMUSTNOTLEAK"));
    }
}
