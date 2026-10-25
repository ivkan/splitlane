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
    append(
        device,
        action,
        serde_json::json!({ "surface_id": surface_id }),
        bytes,
        outcome,
    )
}

/// [`record`] for a session a phone started: which project and which agent,
/// since there is no session to name until it has started.
pub(crate) fn record_open(
    device: &Device,
    project_id: u64,
    agent: &str,
    bytes: usize,
    outcome: &str,
) -> std::io::Result<()> {
    append(
        device,
        "open",
        serde_json::json!({ "project_id": project_id, "agent": agent }),
        bytes,
        outcome,
    )
}

fn append(
    device: &Device,
    action: &str,
    target: serde_json::Value,
    bytes: usize,
    outcome: &str,
) -> std::io::Result<()> {
    let dir = journal_dir().ok_or_else(|| std::io::Error::other("no configuration directory"))?;
    let mut line = serde_json::json!({
        "at_unix": super::now_unix(),
        "device": device.name,
        // Names can repeat and be changed; the key cannot.
        "device_key": fingerprint(&device.public),
        "action": action,
        "bytes": bytes,
        "outcome": outcome,
    });
    if let (Some(line), Some(target)) = (line.as_object_mut(), target.as_object()) {
        line.extend(target.clone());
    }
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
