#![allow(
    clippy::panic,
    reason = "integration test setup failures need contextual diagnostics"
)]

//! The names Splitlane has already published to things outside this process are
//! an invariant, not an implementation detail.
//!
//! The container merge (`workspace` + `project` -> one `project`) renames the
//! *internals*; it must not rename the wire. Three surfaces are already in
//! other people's hands:
//!
//! - `SPLITLANE_WORKSPACE_ID` - injected into every PTY and read back by
//!   `splitlane-shim`, `splitlane-ai-hook` and `splitlane-mcp`. Renaming it breaks
//!   the hook path for every agent already running under an older shim.
//! - The `workspace.*` JSON-RPC methods - called by `splitlane-mcp` and by the
//!   `splitlane` CLI. They stay as the names of the merged-container methods.
//! - The MCP tool names `list_panes` / `read_pane` / `search_pane` - "pane"
//!   reads as "surface" and the tools keep their published names.
//!
//! This test is deliberately a source scan rather than a call: the invariant is
//! about the *literal names* surviving a refactor, and a source scan fails on a
//! rename even when the refactor kept every call site internally consistent.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-app has a parent")
        .to_path_buf()
}

fn read(path: &str) -> String {
    let full = repo_root().join(path);
    std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", full.display()))
}

#[test]
fn pty_env_identity_key_keeps_its_published_name() {
    // Producer: the PTY env assembly that every surface spawns through.
    let pty = read("src-app/src/terminal/pty_session.rs");
    assert!(
        pty.contains("\"SPLITLANE_WORKSPACE_ID\""),
        "the PTY env key `SPLITLANE_WORKSPACE_ID` must keep its name - \
         renaming it breaks splitlane-shim for every agent already launched"
    );

    // Consumers, in the crates that are shipped separately from the app.
    let hook = read("crates/splitlane-ai-hook/src/main.rs");
    assert!(
        hook.contains("\"SPLITLANE_WORKSPACE_ID\""),
        "splitlane-ai-hook reads `SPLITLANE_WORKSPACE_ID`; the name is a contract"
    );
    let mcp = read("crates/splitlane-mcp/src/tools.rs");
    assert!(
        mcp.contains("\"SPLITLANE_WORKSPACE_ID\""),
        "splitlane-mcp reads `SPLITLANE_WORKSPACE_ID`; the name is a contract"
    );
}

#[test]
fn ipc_workspace_methods_survive_the_container_merge() {
    let handler = read("src-app/src/app/ipc_handler.rs");
    // `workspace.*` is what the MCP bridge and the CLI call. After the merge the
    // container behind them is a project; the method names are aliases and stay.
    for method in [
        "workspace.list",
        "workspace.current",
        "workspace.create",
        "workspace.select",
        "workspace.close",
        "workspace.up",
        "workspace.restore_layout",
    ] {
        assert!(
            handler.contains(&format!("\"{method}\"")),
            "IPC method `{method}` disappeared; it is called by splitlane-mcp and \
             the splitlane CLI and must remain as an alias to the project method"
        );
    }
    // `surface.*` already speaks the merged model - nothing to rename, and
    // nothing may be dropped either.
    for method in [
        "surface.list",
        "surface.read",
        "surface.search",
        "surface.focus",
        "surface.rename",
        "surface.status",
        "surface.split",
        "surface.send_text",
        "surface.send_keystroke",
        "app.dispatch_action",
        "surface.send_answer",
    ] {
        assert!(
            handler.contains(&format!("\"{method}\"")),
            "IPC method `{method}` disappeared"
        );
    }
}

#[test]
fn the_rail_keeps_its_published_names() {
    // The object scripts read to learn what a surface is doing, and the event
    // they subscribe to instead of polling for it.
    let handler = read("src-app/src/app/ipc_handler.rs");
    assert!(
        handler.contains("value[\"rail\"]"),
        "`surface.status` must keep answering under the key `rail`"
    );
    let rail = read("src-app/src/rail_state.rs");
    assert!(
        rail.contains("\"surface.rail\""),
        "the event type `surface.rail` is a published name"
    );
    for field in [
        "\"agent\"",
        "\"session_id\"",
        "\"cwd\"",
        "\"status\"",
        "\"source\"",
        "\"tier\"",
        "\"runs_ended\"",
        "\"last_outcome\"",
        "\"turn_marker\"",
        "\"exited\"",
        "\"message\"",
    ] {
        assert!(rail.contains(field), "`rail` lost its field {field}");
    }
    for word in [
        "\"starting\"",
        "\"running\"",
        "\"waiting\"",
        "\"idle\"",
        "\"failed\"",
    ] {
        assert!(rail.contains(word), "the status word {word} is published");
    }
}

#[test]
fn the_wait_exit_codes_keep_their_numbers() {
    // Scripts branch on these. A renumbering compiles and passes every test
    // that reads the constants by name.
    let cli = read("src-app/src/cli/mod.rs");
    for line in [
        "pub const EXIT_TIMEOUT: i32 = 4;",
        "pub const EXIT_NEEDS_PERSON: i32 = 5;",
        "pub const EXIT_AGENT_FAILED: i32 = 6;",
        "pub const EXIT_NO_TURN_SIGNAL: i32 = 7;",
    ] {
        assert!(cli.contains(line), "exit code changed: expected `{line}`");
    }
}

#[test]
fn mcp_bridge_tool_names_keep_reading_pane_as_surface() {
    let tools = read("crates/splitlane-mcp/src/tools.rs");
    for tool in ["list_panes", "read_pane", "search_pane"] {
        assert!(
            tools.contains(&format!("\"{tool}\"")),
            "MCP tool `{tool}` is registered in agent configs on users' machines; \
             `pane` in the name reads as `surface` and the name does not change"
        );
    }
}
