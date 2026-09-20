//! Claude Code writer.
//!
//! Preferred path: shell out to `claude mcp add -s user --transport stdio
//! splitlane -- <bridge>` when the `claude` CLI is on PATH - it owns the
//! schema and writes user-scope servers to `~/.claude.json`. Fallback when
//! `claude` is absent (or the add fails): merge the entry directly into
//! `~/.claude.json` under `mcpServers.splitlane`.
//!
//! The entry carries **no `env` block**: the bridge inherits
//! `SPLITLANE_SOCKET_PATH` from the pane it runs in. Per 2026 verification
//! the entry also carries `type: "stdio"`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::agents::{support, AgentConfigWriter, InstallOutcome, StatusOutcome, UninstallOutcome};
use crate::detect::{self, Presence};
use crate::{io, merge};

const CLI: &str = "claude";
const CONTAINER: &str = "mcpServers";

pub struct ClaudeCode {
    config_path: Option<PathBuf>,
    /// Whether shell-out to the `claude` CLI is permitted. Always true in
    /// production; forced false in unit tests so they never mutate the
    /// developer's real `~/.claude.json` via a real `claude` on PATH.
    allow_cli: bool,
}

impl ClaudeCode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config_path: support::claude_config(),
            allow_cli: true,
        }
    }

    fn path(&self) -> Result<&Path> {
        self.config_path
            .as_deref()
            .ok_or_else(|| anyhow!("cannot resolve home dir for ~/.claude.json"))
    }

    fn entry(bridge: &str) -> serde_json::Value {
        // No `env`. `type: "stdio"` matches what `claude mcp add` writes.
        json!({ "type": "stdio", "command": bridge, "args": [] })
    }

    /// True for either shape a healthy entry can have, which is two because
    /// there are two writers.
    ///
    /// `install` prefers shelling out to `claude mcp add` over editing
    /// `~/.claude.json` ourselves, on the reasoning that the CLI owns its own
    /// file's conventions. It does - and one of them is that it writes an
    /// empty `env: {}` where `entry()` writes no `env` at all. Comparing
    /// against `entry()` alone therefore answered `needs repair` immediately
    /// after a *successful* install, on every machine with `claude` on PATH:
    /// two writers, one validator, and the validator only knew one of them.
    /// Measured 6 September 2026 against Claude Code 2.1.259.
    ///
    /// The tolerance is exactly that difference and no wider. An **empty**
    /// `env` is the CLI's own spelling of "no environment" and is accepted; a
    /// **populated** one is still a repair, because then something really did
    /// put variables into the bridge's environment and we did not.
    fn shape_matches(entry: &serde_json::Value, bridge: &str) -> bool {
        let mut normalised = entry.clone();
        if let Some(obj) = normalised.as_object_mut() {
            let env_is_empty = obj
                .get("env")
                .and_then(serde_json::Value::as_object)
                .is_some_and(serde_json::Map::is_empty);
            if env_is_empty {
                obj.remove("env");
            }
        }
        normalised == Self::entry(bridge)
    }

    fn validate_entry(entry: &serde_json::Value, expected: Option<&Path>) -> StatusOutcome {
        let found = support::string_command(entry);
        let shape_ok = found
            .as_deref()
            .is_some_and(|path| Self::shape_matches(entry, path));
        support::classify_entry(
            found,
            expected,
            shape_ok,
            "Claude Code MCP entry must be stdio, have empty args, and no populated env block",
        )
    }
}

impl Default for ClaudeCode {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentConfigWriter for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude-code"
    }
    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn presence(&self) -> Presence {
        let cli = if self.allow_cli { Some(CLI) } else { None };
        let paths: Vec<PathBuf> = self.config_path.clone().into_iter().collect();
        detect::detect(cli, &paths)
    }

    fn install(&self, bridge: &Path) -> Result<InstallOutcome> {
        let path = self.path()?;
        let bridge_s = bridge.to_string_lossy().into_owned();

        // Idempotency + update detection via the same file the CLI writes.
        // This validates the whole managed entry, not just the command path.
        let status = support::json_status(path, CONTAINER, Some(bridge), Self::validate_entry)?;
        if matches!(status, StatusOutcome::Installed { .. }) {
            return Ok(InstallOutcome::AlreadyCurrent);
        }
        let had_prior = support::json_entry_present(path, CONTAINER)?;

        if self.allow_cli && support::cli_on_path(CLI) {
            io::backup(path)?;
            // A stale entry would make `add` conflict; remove it first
            // (best-effort - a missing entry just no-ops).
            if had_prior {
                let _ = support::shell_out(CLI, &["mcp", "remove", "splitlane"]);
            }
            match support::shell_out(
                CLI,
                &[
                    "mcp",
                    "add",
                    "-s",
                    "user",
                    "--transport",
                    "stdio",
                    "splitlane",
                    "--",
                    &bridge_s,
                ],
            ) {
                Ok(()) => {
                    return Ok(if had_prior {
                        InstallOutcome::Updated
                    } else {
                        InstallOutcome::Installed
                    });
                }
                Err(e) => {
                    log::warn!(
                        "splitlane mcp: `claude mcp add` failed ({e:#}); falling back to direct ~/.claude.json merge"
                    );
                }
            }
        }

        support::json_install(path, CONTAINER, Self::entry(&bridge_s))
    }

    fn uninstall(&self) -> Result<UninstallOutcome> {
        let path = self.path()?;
        // A present-but-unparseable `~/.claude.json` must surface a
        // loud error, not be silently mistaken for "nothing to remove". The
        // tolerant `current_json_command` below swallows parse failures
        // (`.ok()?` → None), so probe parseability first - `read_json_or_default`
        // is `Err` on a present malformed file and `Ok` (skeleton) when absent.
        if path.exists() {
            merge::read_json_or_default(path)?;
        }
        if !support::json_entry_present(path, CONTAINER)? {
            return Ok(UninstallOutcome::NothingToRemove);
        }
        if self.allow_cli && support::cli_on_path(CLI) {
            io::backup(path)?;
            if let Ok(()) = support::shell_out(CLI, &["mcp", "remove", "splitlane"]) {
                return Ok(UninstallOutcome::Removed);
            }
        }
        support::json_uninstall(path, CONTAINER)
    }

    fn status(&self, bridge: Option<&Path>) -> Result<StatusOutcome> {
        support::json_status(self.path()?, CONTAINER, bridge, Self::validate_entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_writer(path: PathBuf) -> ClaudeCode {
        ClaudeCode {
            config_path: Some(path),
            allow_cli: false, // never shell out to a real `claude` in tests
        }
    }

    #[test]
    fn install_writes_stdio_entry_without_env() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join(".claude.json");
        let w = test_writer(p.clone());

        assert_eq!(
            w.install(Path::new("/data/splitlane-mcp")).unwrap(),
            InstallOutcome::Installed
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        let entry = &v["mcpServers"]["splitlane"];
        assert_eq!(entry["type"], json!("stdio"));
        assert_eq!(entry["command"], json!("/data/splitlane-mcp"));
        assert_eq!(entry["args"], json!([]));
        assert!(entry.get("env").is_none(), "entry must carry no env block");
    }

    /// The shape `claude mcp add` writes, recorded from a real run rather than
    /// guessed at: it is `entry()` plus an empty `env`. Since `install`
    /// prefers that CLI whenever `claude` is on PATH, this is the shape most
    /// installed machines actually have, and `status` used to call it broken.
    ///
    /// The literal is written out here rather than produced by shelling out to
    /// `claude`, because a test that needs the real CLI installed does not run
    /// in CI - and this defect is precisely one that only appears on the path
    /// CI never takes. Re-measure the literal if Claude Code changes it.
    #[test]
    fn status_accepts_the_shape_the_claude_cli_writes() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "mcpServers": {
                    "splitlane": {
                        "type": "stdio",
                        "command": "/data/splitlane-mcp",
                        "args": [],
                        "env": {}
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let w = test_writer(p);

        assert!(
            matches!(
                w.status(Some(Path::new("/data/splitlane-mcp"))).unwrap(),
                StatusOutcome::Installed { .. }
            ),
            "an empty `env` is the CLI's spelling of `no environment`; \
             reporting it as needing repair told the user that a successful \
             install had failed"
        );
    }

    /// The other half of the same rule: the tolerance is for an EMPTY `env`
    /// and nothing wider. Variables in there were put there by someone else,
    /// and the bridge would run with an environment we did not choose.
    #[test]
    fn status_still_repairs_a_populated_env() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "mcpServers": {
                    "splitlane": {
                        "type": "stdio",
                        "command": "/data/splitlane-mcp",
                        "args": [],
                        "env": { "HTTP_PROXY": "http://somewhere" }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let w = test_writer(p);

        assert!(matches!(
            w.status(Some(Path::new("/data/splitlane-mcp"))).unwrap(),
            StatusOutcome::NeedsRepair { .. }
        ));
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let w = test_writer(dir.path().join(".claude.json"));
        w.install(Path::new("/data/splitlane-mcp")).unwrap();
        assert_eq!(
            w.install(Path::new("/data/splitlane-mcp")).unwrap(),
            InstallOutcome::AlreadyCurrent
        );
    }

    #[test]
    fn status_needs_repair_when_shape_differs() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "mcpServers": {
                    "splitlane": {
                        "type": "stdio",
                        "command": "/data/splitlane-mcp",
                        "args": [],
                        "env": { "SHOULD_NOT_BE_HERE": "1" }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let w = test_writer(p);

        assert!(matches!(
            w.status(Some(Path::new("/data/splitlane-mcp"))).unwrap(),
            StatusOutcome::NeedsRepair { .. }
        ));
    }

    #[test]
    fn install_preserves_unrelated_claude_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({
                "numStartups": 42,
                "mcpServers": { "github": { "command": "gh-mcp" } }
            }))
            .unwrap(),
        )
        .unwrap();
        let w = test_writer(p.clone());
        w.install(Path::new("/data/splitlane-mcp")).unwrap();

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["numStartups"], json!(42));
        assert_eq!(v["mcpServers"]["github"]["command"], json!("gh-mcp"));
        assert_eq!(
            v["mcpServers"]["splitlane"]["command"],
            json!("/data/splitlane-mcp")
        );
    }

    #[test]
    fn uninstall_malformed_config_is_error() {
        // A present-but-unparseable config is corruption, not
        // "nothing to remove" - surface a loud error so the user fixes it
        // rather than silently believing the entry was already gone.
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(&p, b"{ broken").unwrap();
        let w = test_writer(p.clone());
        assert!(
            w.uninstall().is_err(),
            "uninstall on a malformed present config must error, not return NothingToRemove"
        );
        // The invalid file was NOT overwritten.
        assert_eq!(std::fs::read(&p).unwrap(), b"{ broken");
    }

    #[test]
    fn uninstall_absent_config_is_nothing_to_remove() {
        // Counterpart to the malformed case: a genuinely absent file is a
        // clean NothingToRemove, not an error.
        let dir = tempfile::TempDir::new().unwrap();
        let w = test_writer(dir.path().join("missing.json"));
        assert_eq!(w.uninstall().unwrap(), UninstallOutcome::NothingToRemove);
    }

    #[test]
    fn uninstall_then_status_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join(".claude.json");
        let w = test_writer(p);
        w.install(Path::new("/data/splitlane-mcp")).unwrap();
        assert_eq!(
            w.status(Some(Path::new("/data/splitlane-mcp"))).unwrap(),
            StatusOutcome::Installed {
                path: "/data/splitlane-mcp".into()
            }
        );
        assert_eq!(w.uninstall().unwrap(), UninstallOutcome::Removed);
        assert_eq!(
            w.status(Some(Path::new("/data/splitlane-mcp"))).unwrap(),
            StatusOutcome::NotInstalled
        );
    }
}
