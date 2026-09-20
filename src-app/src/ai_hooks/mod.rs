//! Cross-platform AI-hook wiring.
//!
//! Binary extraction & cache-dir layout.
//!   `extract::ensure_binaries_extracted` materializes the embedded
//!   `splitlane-shim` and `splitlane-ai-hook` binaries into
//!   `<cache_dir>/splitlane/bin/<version>/` with atomic rename + `chmod
//!   0o755` on Unix. The shim is written twice (as `claude` and `codex`)
//!   so the PTY's `$PATH`-prepend in `pty_session` resolves both tool names
//!   to the same underlying shim.
//!
//! MCP bridge extraction.
//!   `extract::ensure_bridge_extracted` materializes the embedded
//!   `splitlane-mcp` bridge to a stable, non-versioned path under
//!   `data_dir()` (`runtime_paths::bridge_binary_path`), distinct from the
//!   version-pinned helper cache above. Called once at launch from
//!   `main()`; the path is what `splitlane mcp install` writes into
//!   agent configs, so it must survive Splitlane updates.

pub mod extract;

use std::io::Write;

/// Opt-in cross-process diagnostic for the sidebar-status hook chain.
///
/// Appends one line to the file named by `$SPLITLANE_HOOK_LOG` when that env
/// var is set and non-empty; a silent no-op otherwise. This is the SAME env
/// var honoured by the `splitlane-shim` and `splitlane-ai-hook` binaries, so
/// the whole pipeline - app (PTY env + IPC server) → shell → shim → agent →
/// ai-hook - appends to one file. That lets a user trace exactly where the
/// chain breaks on Windows (e.g. shim never runs vs. hooks never install vs.
/// frame never reaches the server) from a single reproduction.
///
/// To capture: set `SPLITLANE_HOOK_LOG` (e.g. in PowerShell
/// `$env:SPLITLANE_HOOK_LOG = "C:\Users\<you>\splitlane-hooks.log"`), launch
/// Splitlane from that same shell so it inherits the var, run an agent, then
/// share the file. Never panics - diagnostics must never break a PTY spawn.
pub(crate) fn hook_diag(msg: &str) {
    let Some(path) = std::env::var_os("SPLITLANE_HOOK_LOG") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    // Format the whole line first and emit it in ONE `write_all`: multiple
    // processes (app, shim, ai-hook ×N) append to this file concurrently, and
    // a single atomic append keeps lines from interleaving/dropping (a
    // per-argument `writeln!` issues several syscalls and tears under
    // concurrency).
    let line = format!("splitlane-app[{}]: {msg}\n", std::process::id());
    let _ = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}
