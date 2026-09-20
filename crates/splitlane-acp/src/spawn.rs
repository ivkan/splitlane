//! `CLAUDECODE` env scrub helpers.
//!
//! The ACP agent-spawn machinery (`spawn_acp_agent`, wire tracing,
//! secret redaction) was removed with the in-app chat; only this env
//! scrub survives, called once from `main()` before any thread spawns.

/// Environment variable set by a running Claude Code process. If
/// inherited by a child `claude` / `claude-code-acp` wrapper, the
/// wrapper refuses to launch ("Claude Code cannot launch inside another
/// Claude Code session").
const CLAUDECODE_ENV: &str = "CLAUDECODE";

/// Every agent-session marker Splitlane refuses to hand down to a spawned
/// terminal, `CLAUDECODE` included.
///
/// `CLAUDE_CODE_CHILD_SESSION` is the costly one: an agent that inherits it
/// believes it is a nested child session and **turns transcript saving off**,
/// so its conversation never reaches `~/.claude/projects` and the session can
/// never be resumed. The only warning is one dim line at the bottom of the
/// agent's own TUI. The messaging socket/token pair is stripped too - it is a
/// live credential for the launching session's IPC channel.
///
/// The last three arrived with Claude Code 2.1.27x and are measured harmless to
/// transcript saving and to the status file (19 September 2026, 2.1.278). They
/// are stripped because they describe the *launching* session, not the pane's:
/// `CLAUDE_EFFORT` would hand that session's effort setting to every agent
/// started in a pane, and `CLAUDE_PID` names a process the pane's agent is not.
///
/// These have to leave *this process's* environment, not merely the env map
/// Splitlane layers onto a PTY: the terminal backend spawns children against the
/// inherited environment with no `env_clear`, so a key that is simply absent
/// from the overlay is still passed straight through.
pub const INHERITED_AGENT_SESSION_ENV: &[&str] = &[
    CLAUDECODE_ENV,
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// Host-terminal identity markers Splitlane inherits from whatever launched it.
///
/// A pane must look like a fresh terminal, never like the terminal that
/// happened to start Splitlane. These are how TUI programs answer "which
/// emulator am I talking to", and the pane already answers that through `TERM`
/// / `TERM_PROGRAM`. A stale one makes the child believe something else, and
/// two agent CLIs change their wire behaviour on exactly these (reported
/// upstream, not re-measured here):
///
/// - `WT_SESSION`: Claude Code disables its OSC 9;4 progress reporting when it
///   is set, so a Splitlane launched from Windows Terminal loses that channel
///   in every pane.
/// - `TMUX` / `STY` / `ZELLIJ`: Claude Code and Codex wrap their OSC
///   notifications in multiplexer passthrough, which neither backend unwraps.
/// - The rest are the same class of capability probe with no known breakage,
///   dropped because the answer they give is about a terminal that is not
///   rendering the pane.
///
/// `ConEmu*` is matched by prefix (`ConEmuANSI`, `ConEmuPID`, ...). Matching
/// is ASCII-case-insensitive because these arrive by inheritance, in whatever
/// casing the host set them.
pub const INHERITED_HOST_TERMINAL_ENV: &[&str] = &[
    "WT_SESSION",
    "WT_PROFILE_ID",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "KITTY_WINDOW_ID",
    "KITTY_LISTEN_ON",
    "TERMINAL_EMULATOR",
    "VTE_VERSION",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
];

const CONEMU_ENV_PREFIX: &str = "conemu";

/// True if `key` names a host-terminal identity marker - see
/// [`INHERITED_HOST_TERMINAL_ENV`]. Pure, so the list is tested without
/// touching the process environment.
pub fn is_inherited_host_terminal_env_key(key: &str) -> bool {
    INHERITED_HOST_TERMINAL_ENV
        .iter()
        .any(|known| key.eq_ignore_ascii_case(known))
        || (key.len() > CONEMU_ENV_PREFIX.len()
            && key.is_char_boundary(CONEMU_ENV_PREFIX.len())
            && key[..CONEMU_ENV_PREFIX.len()].eq_ignore_ascii_case(CONEMU_ENV_PREFIX))
}

/// Remove `CLAUDECODE` from the current process environment so future
/// subprocesses (which inherit it by default) do not see it.
///
/// This helper MUST be
/// called from the very first lines of `main()`, before any
/// `std::thread::spawn`, `tokio::runtime::Builder::build`, or smol
/// executor initialization. Rust 1.85 made `std::env::remove_var`
/// `unsafe` because it races with concurrent `getenv` from any
/// other thread; the runtime sub-systems above all read env on
/// startup, so calling this before any thread exists is genuinely safe
/// by construction.
/// # Safety
///
/// Must be called before any other thread, async runtime, or foreign library
/// can concurrently read environment variables. Prefer
/// [`scrub_claudecode_from_command`] for per-child scrubbing after startup.
pub unsafe fn scrub_claudecode_env() {
    // SAFETY: called from main() before any thread::spawn or async
    // runtime init -- no concurrent getenv possible.
    unsafe {
        std::env::remove_var(CLAUDECODE_ENV);
    }
}

/// Remove every [`INHERITED_AGENT_SESSION_ENV`] marker from the current process
/// environment so subprocesses - which inherit it by default - never see them.
///
/// Superset of [`scrub_claudecode_env`]; both carry the same
/// call-before-any-thread requirement.
///
/// # Safety
///
/// Must be called before any other thread, async runtime, or foreign library
/// can concurrently read environment variables.
pub unsafe fn scrub_inherited_agent_session_env() {
    for key in INHERITED_AGENT_SESSION_ENV {
        // SAFETY: caller guarantees no other thread exists yet (see the
        // function contract and the call site at the top of `main`).
        unsafe {
            std::env::remove_var(key);
        }
    }
}

/// Remove every host-terminal identity marker (see
/// [`INHERITED_HOST_TERMINAL_ENV`]) from the current process environment, for
/// the same reason and by the same route as
/// [`scrub_inherited_agent_session_env`]: the terminal backends spawn against
/// the inherited environment, and only removing the key here keeps it out of
/// every pane on both of them.
///
/// # Safety
///
/// Must be called before any other thread, async runtime, or foreign library
/// can concurrently read environment variables.
pub unsafe fn scrub_inherited_host_terminal_env() {
    let keys: Vec<std::ffi::OsString> = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_str().is_some_and(is_inherited_host_terminal_env_key))
        .collect();
    for key in keys {
        // SAFETY: caller guarantees no other thread exists yet.
        unsafe {
            std::env::remove_var(key);
        }
    }
}

/// Remove `CLAUDECODE` from one child command without mutating global process
/// environment.
pub fn scrub_claudecode_from_command(command: &mut std::process::Command) {
    command.env_remove(CLAUDECODE_ENV);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_terminal_markers_are_recognized_whatever_their_casing() {
        for key in INHERITED_HOST_TERMINAL_ENV {
            assert!(is_inherited_host_terminal_env_key(key), "{key}");
            assert!(
                is_inherited_host_terminal_env_key(&key.to_lowercase()),
                "{key} must be recognized case-insensitively"
            );
        }
        for key in ["ConEmuANSI", "ConEmuPID", "ConEmuTask", "CONEMUBUILD"] {
            assert!(is_inherited_host_terminal_env_key(key), "{key}");
        }
    }

    #[test]
    fn host_terminal_matcher_does_not_swallow_unrelated_names() {
        for key in [
            "conemu",
            "CONEMU",
            "TERM",
            "TERM_PROGRAM",
            "TMUXINATOR_CONFIG",
            "STYLE",
            "PATH",
            "KITTY_WINDOW_IDS",
            "SPLITLANE_WORKSPACE_ID",
            "CLAUDE_CONFIG_DIR",
        ] {
            assert!(
                !is_inherited_host_terminal_env_key(key),
                "{key} is not a host-terminal identity marker"
            );
        }
    }

    #[test]
    fn scrub_claudecode_is_idempotent() {
        // SAFETY: test-only -- single-threaded test runner step. Sets, scrubs,
        // and re-scrubs to confirm the second call does not panic.
        unsafe {
            std::env::set_var(CLAUDECODE_ENV, "1");
        }
        // SAFETY: this test holds no extra threads and only exercises this env var.
        unsafe { scrub_claudecode_env() };
        assert!(std::env::var(CLAUDECODE_ENV).is_err());
        // SAFETY: same as above.
        unsafe { scrub_claudecode_env() };
        assert!(std::env::var(CLAUDECODE_ENV).is_err());
    }

    #[test]
    fn scrub_inherited_removes_every_marker() {
        // Deliberately leaves `CLAUDECODE` alone: `scrub_claudecode_is_idempotent`
        // owns that key, and the test harness runs both on parallel threads.
        let owned: Vec<&str> = INHERITED_AGENT_SESSION_ENV
            .iter()
            .copied()
            .filter(|key| *key != CLAUDECODE_ENV)
            .collect();
        assert!(
            owned.contains(&"CLAUDE_CODE_CHILD_SESSION"),
            "the marker that disables transcript saving must be scrubbed"
        );
        for key in &owned {
            // SAFETY: test-only; these keys are touched by no other test.
            unsafe {
                std::env::set_var(key, "1");
            }
        }

        // SAFETY: test-only -- see above.
        unsafe { scrub_inherited_agent_session_env() };

        for key in &owned {
            assert!(
                std::env::var(key).is_err(),
                "{key} must not survive the scrub"
            );
        }
    }

    #[test]
    fn scrub_claudecode_from_command_is_local_to_child() {
        let mut command = std::process::Command::new("noop");
        command.env(CLAUDECODE_ENV, "1");
        scrub_claudecode_from_command(&mut command);
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == CLAUDECODE_ENV && value.is_none()),
            "child command should explicitly remove CLAUDECODE"
        );
    }
}
