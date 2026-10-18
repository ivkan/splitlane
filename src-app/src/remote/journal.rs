//! What paired phones wrote, one line each, in `remote/journal.log`.
//!
//! Only what happened, never what was written: text sent to an agent often
//! carries secrets, and a log of it would be one more place they sit.

use sha2::{Digest, Sha256};

use super::store::{self, Device};

/// Append one line to the journal: `asked` before a write, which the
/// caller does not make when this fails, and `done` or `refused` after.
pub(crate) fn record(
    device: &Device,
    action: &str,
    surface_id: u64,
    bytes: usize,
    outcome: &str,
) -> std::io::Result<()> {
    let dir = journal_dir().ok_or_else(|| std::io::Error::other("no configuration directory"))?;
    let line = serde_json::json!({
        "at_unix": super::now_unix(),
        "device": device.name,
        // Names can repeat and be changed; the key cannot.
        "device_key": fingerprint(&device.public),
        "action": action,
        "surface_id": surface_id,
        "bytes": bytes,
        "outcome": outcome,
    });
    store::append_private(&dir, "journal.log", format!("{line}\n").as_bytes())
}

/// The first 16 hex digits of the SHA-256 of a device's public key.
fn fingerprint(public: &str) -> String {
    Sha256::digest(public.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(not(test))]
fn journal_dir() -> Option<std::path::PathBuf> {
    store::dir()
}

/// Tests write their journal lines where nobody's configuration is.
#[cfg(test)]
fn journal_dir() -> Option<std::path::PathBuf> {
    Some(std::env::temp_dir().join(format!("splitlane-journal-test-{}", std::process::id())))
}
