#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unwrap_in_result,
        clippy::panic
    )
)]
//! splitlane-mcp - MCP (Model Context Protocol) stdio bridge for Splitlane.
//!
//! Lets an MCP-capable CLI agent (Claude Code, Codex, Gemini CLI, opencode)
//! running inside a Splitlane pane read the terminal output of ANY other
//! surface. It speaks MCP over stdin/stdout (the agent spawns it as a
//! subprocess) and proxies each tool call to Splitlane's local IPC socket.
//!
//! Tools (all READ-ONLY): `list_panes`, `read_pane`, `search_pane`. There is
//! deliberately no write/keystroke tool - the IPC scripting gate stays the
//! sole, opt-in write surface (a security decision).
//!
//! Module map:
//! - [`splitlane_ipc_client`] - socket path resolution + blocking JSON-RPC
//!   client, shared with the `splitlane` CLI.
//! - [`mcp`] - MCP stdio protocol loop
//! - [`tools`] - the three tools + untrusted-output wrapping
//! - [`resolve`] - name → surface_id resolution with disambiguation

mod mcp;
mod resolve;
mod tools;

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(socket) = splitlane_ipc_client::resolve_socket_path() else {
        eprintln!(
            "splitlane-mcp: cannot locate the Splitlane IPC socket. \
             Set SPLITLANE_SOCKET_PATH (normally inherited from the Splitlane PTY) \
             or launch this bridge from inside a Splitlane pane."
        );
        return ExitCode::FAILURE;
    };

    let client = splitlane_ipc_client::IpcClient::new(socket);
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    let scope = tools::BridgeScope::from_env();

    match mcp::serve(stdin, stdout, &client, scope) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("splitlane-mcp: stdio loop terminated: {e}");
            ExitCode::FAILURE
        }
    }
}
