//! The permissions profile that lets a Codex session reach Splitlane.
//!
//! Codex runs every command of a turn in a sandbox, and that sandbox refuses
//! a connection to a Unix socket. So `splitlane` run by Codex cannot reach
//! the app, and a Codex session cannot open or read other sessions.
//!
//! Codex has one way to allow a socket: a permissions profile whose network
//! is enabled, with the socket listed under it. Measured on Codex 0.158 with
//! live turns: a list of sockets alone opens nothing, and with the network
//! enabled the commands of a turn reach the whole internet - a list of
//! allowed domains did not narrow it. **Allowing the socket therefore costs
//! the network**, and that is why nothing here is written without the person
//! asking for it and reading what it costs.
//!
//! The profile is named, extends Codex's own workspace profile, and is made
//! the default. A config that already names a default profile, or sets the
//! older `sandbox_mode`, is left alone: the person chose that, and replacing
//! their choice with ours would change what Codex may do in ways they did not
//! ask for.

use std::path::{Path, PathBuf};

use anyhow::Result;
use toml_edit::{value, DocumentMut, Item, Table};

/// The profile's name in Codex's config.
pub const PROFILE: &str = "splitlane";

/// Codex's built-in profile ours extends: write inside the project, read
/// elsewhere.
const EXTENDS: &str = ":workspace";

/// What the config says about our profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Our profile is the default and allows this socket.
    Enabled,
    /// Our profile is not the default.
    Disabled,
    /// The config makes its own choice, named here, and ours cannot be added
    /// without replacing it.
    Conflict(String),
}

/// Why a profile was not written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The config already chooses how Codex is sandboxed; the text names the
    /// key and its value.
    OwnChoice(String),
    /// The file could not be read or written.
    Io(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::OwnChoice(what) => write!(
                f,
                "Codex's config already sets {what}; Splitlane left it alone"
            ),
            Refusal::Io(error) => write!(f, "{error}"),
        }
    }
}

/// `config.toml` in Codex's home: `CODEX_HOME` when set, `~/.codex` otherwise.
pub fn config_path(
    home: Option<PathBuf>,
    codex_home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    codex_home
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".codex")))
        .map(|dir| dir.join("config.toml"))
}

/// A choice in the config that is not ours.
fn own_choice(doc: &DocumentMut) -> Option<String> {
    if let Some(default) = doc.get("default_permissions").and_then(Item::as_str) {
        if default != PROFILE {
            return Some(format!("default_permissions = \"{default}\""));
        }
    }
    doc.get("sandbox_mode")
        .and_then(Item::as_str)
        .map(|mode| format!("sandbox_mode = \"{mode}\""))
}

fn profile_allows(doc: &DocumentMut, socket: &str) -> bool {
    let Some(network) = doc
        .get("permissions")
        .and_then(|item| item.get(PROFILE))
        .and_then(|item| item.get("network"))
    else {
        return false;
    };
    let enabled = network.get("enabled").and_then(Item::as_bool) == Some(true);
    let listed = network
        .get("unix_sockets")
        .and_then(|item| item.get(socket))
        .and_then(Item::as_str)
        == Some("allow");
    enabled && listed
}

/// What `doc` says about our profile and `socket`.
pub fn status_of(doc: &DocumentMut, socket: &str) -> Status {
    if let Some(choice) = own_choice(doc) {
        return Status::Conflict(choice);
    }
    let ours = doc.get("default_permissions").and_then(Item::as_str) == Some(PROFILE);
    if ours && profile_allows(doc, socket) {
        Status::Enabled
    } else {
        Status::Disabled
    }
}

/// Add the profile and make it the default. Refuses a config that already
/// chooses its own sandboxing.
pub fn enable_in(doc: &mut DocumentMut, socket: &str) -> Result<(), Refusal> {
    if let Some(choice) = own_choice(doc) {
        return Err(Refusal::OwnChoice(choice));
    }
    // The profile is ours whole, so it is built afresh rather than patched:
    // a socket allowed by an older write does not stay allowed.
    let mut sockets = Table::new();
    sockets[socket] = value("allow");
    let mut network = Table::new();
    network["enabled"] = value(true);
    network["unix_sockets"] = Item::Table(sockets);
    let mut profile = Table::new();
    profile["extends"] = value(EXTENDS);
    profile["network"] = Item::Table(network);

    match doc.get_mut("permissions") {
        None => {
            let mut permissions = Table::new();
            // The parent of a dotted table path prints no header of its own.
            permissions.set_implicit(true);
            permissions.insert(PROFILE, Item::Table(profile));
            doc.insert("permissions", Item::Table(permissions));
        }
        Some(item) => match item.as_table_mut() {
            Some(permissions) => {
                permissions.insert(PROFILE, Item::Table(profile));
            }
            // Written inline, or not a table at all. Rewriting it would
            // reshape something the person wrote by hand.
            None => {
                return Err(Refusal::OwnChoice(
                    "permissions in a form Splitlane does not edit".to_string(),
                ));
            }
        },
    }
    doc["default_permissions"] = value(PROFILE);
    Ok(())
}

/// Take the profile out, and the default with it when the default is ours.
/// Returns whether anything changed.
pub fn disable_in(doc: &mut DocumentMut) -> bool {
    let mut changed = false;
    if doc.get("default_permissions").and_then(Item::as_str) == Some(PROFILE) {
        doc.remove("default_permissions");
        changed = true;
    }
    if let Some(permissions) = doc.get_mut("permissions").and_then(Item::as_table_mut) {
        changed |= permissions.remove(PROFILE).is_some();
        if permissions.is_empty() {
            doc.remove("permissions");
        }
    }
    changed
}

fn read(path: &Path) -> Result<DocumentMut, Refusal> {
    crate::merge::read_toml_or_default(path).map_err(|error| Refusal::Io(format!("{error:#}")))
}

/// What the config at `path` says. A file that cannot be read says nothing.
pub fn status(path: &Path, socket: &str) -> Status {
    match read(path) {
        Ok(doc) => status_of(&doc, socket),
        Err(_) => Status::Disabled,
    }
}

/// Write the profile into the config at `path`.
pub fn enable(path: &Path, socket: &str) -> Result<(), Refusal> {
    let mut doc = read(path)?;
    enable_in(&mut doc, socket)?;
    write(path, &doc)
}

/// Remove the profile from the config at `path`. A config without it, or no
/// config at all, is left untouched.
pub fn disable(path: &Path) -> Result<(), Refusal> {
    if !path.exists() {
        return Ok(());
    }
    let mut doc = read(path)?;
    if !disable_in(&mut doc) {
        return Ok(());
    }
    write(path, &doc)
}

fn write(path: &Path, doc: &DocumentMut) -> Result<(), Refusal> {
    crate::io::write_if_changed(path, &crate::merge::toml_to_bytes(doc))
        .map(|_| ())
        .map_err(|error| Refusal::Io(format!("{error:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOCKET: &str = "/tmp/splitlane/splitlane.sock";

    fn doc(text: &str) -> DocumentMut {
        text.parse().expect("valid toml")
    }

    #[test]
    fn enabling_adds_the_profile_and_keeps_what_was_there() {
        let mut config = doc("model = \"gpt-5\"\n\n[tui]\ntheme = \"dark\"\n");
        assert_eq!(status_of(&config, SOCKET), Status::Disabled);
        enable_in(&mut config, SOCKET).expect("nothing in the way");
        assert_eq!(status_of(&config, SOCKET), Status::Enabled);

        let text = config.to_string();
        assert!(text.contains("model = \"gpt-5\""), "{text}");
        assert!(text.contains("theme = \"dark\""), "{text}");
        // What Codex reads, parsed back rather than matched as text.
        let parsed = doc(&text);
        assert_eq!(parsed["default_permissions"].as_str(), Some(PROFILE));
        let profile = &parsed["permissions"][PROFILE];
        assert_eq!(profile["extends"].as_str(), Some(":workspace"));
        assert_eq!(profile["network"]["enabled"].as_bool(), Some(true));
        assert_eq!(
            profile["network"]["unix_sockets"][SOCKET].as_str(),
            Some("allow")
        );
    }

    #[test]
    fn disabling_puts_the_config_back() {
        let before = "model = \"gpt-5\"\n\n[tui]\ntheme = \"dark\"\n";
        let mut config = doc(before);
        enable_in(&mut config, SOCKET).expect("enabled");
        assert!(disable_in(&mut config));
        assert_eq!(config.to_string(), before);
        assert!(!disable_in(&mut config), "nothing left to take out");
    }

    #[test]
    fn a_config_with_its_own_sandboxing_is_left_alone() {
        for (text, named) in [
            (
                "default_permissions = \"mine\"\n",
                "default_permissions = \"mine\"",
            ),
            (
                "sandbox_mode = \"danger-full-access\"\n",
                "sandbox_mode = \"danger-full-access\"",
            ),
        ] {
            let mut config = doc(text);
            assert_eq!(
                status_of(&config, SOCKET),
                Status::Conflict(named.to_string())
            );
            assert_eq!(
                enable_in(&mut config, SOCKET),
                Err(Refusal::OwnChoice(named.to_string()))
            );
            assert_eq!(config.to_string(), text, "untouched");
            assert!(!disable_in(&mut config), "and not ours to remove");
            assert_eq!(config.to_string(), text);
        }
    }

    #[test]
    fn another_profile_survives_ours_being_removed() {
        let mut config = doc("[permissions.mine]\nextends = \":read-only\"\n");
        enable_in(&mut config, SOCKET).expect("no default was named");
        assert!(disable_in(&mut config));
        let parsed = doc(&config.to_string());
        assert_eq!(
            parsed["permissions"]["mine"]["extends"].as_str(),
            Some(":read-only")
        );
        assert!(parsed.get("default_permissions").is_none());
    }

    #[test]
    fn a_moved_socket_is_not_enabled_and_enabling_replaces_the_old_one() {
        let mut config = doc("");
        enable_in(&mut config, "/old/splitlane.sock").expect("enabled");
        assert_eq!(status_of(&config, SOCKET), Status::Disabled);
        enable_in(&mut config, SOCKET).expect("ours may be rewritten");
        assert_eq!(status_of(&config, SOCKET), Status::Enabled);
        assert!(
            !config.to_string().contains("/old/splitlane.sock"),
            "the old socket is no longer allowed"
        );
    }

    #[test]
    fn the_config_is_found_in_codex_home_or_under_the_home_directory() {
        assert_eq!(
            config_path(Some("/home/a".into()), None),
            Some(PathBuf::from("/home/a/.codex/config.toml"))
        );
        assert_eq!(
            config_path(Some("/home/a".into()), Some("/elsewhere".into())),
            Some(PathBuf::from("/elsewhere/config.toml"))
        );
        assert_eq!(
            config_path(Some("/home/a".into()), Some("".into())),
            Some(PathBuf::from("/home/a/.codex/config.toml"))
        );
        assert_eq!(config_path(None, None), None);
    }

    #[test]
    fn enabling_and_disabling_a_file_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        // No file is a config with nothing in it.
        assert_eq!(status(&path, SOCKET), Status::Disabled);
        disable(&path).expect("nothing to do");
        assert!(!path.exists(), "disabling does not create a config");

        enable(&path, SOCKET).expect("written");
        assert_eq!(status(&path, SOCKET), Status::Enabled);
        disable(&path).expect("removed");
        assert_eq!(status(&path, SOCKET), Status::Disabled);
    }
}
