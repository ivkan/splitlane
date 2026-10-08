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

    // Read by scripts and by the skill, which are not in this repository's
    // build: nothing here fails to compile when the name changes.
    assert!(
        pty.contains("env.insert(\"SPLITLANE_CLI\".into()"),
        "every pane exports `SPLITLANE_CLI`, the path of the CLI it belongs to"
    );
    assert!(
        read("skills/splitlane-fleet/SKILL.md").contains("\"$SPLITLANE_CLI\""),
        "the skill falls back on `SPLITLANE_CLI` where `splitlane` is not on PATH"
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
fn opening_a_session_keeps_its_published_names() {
    // What a session calls to open another, what it is told back, and the
    // words it is refused with. An agent's instructions are written against
    // all three.
    let handler = read("src-app/src/app/ipc_handler.rs");
    assert!(
        handler.contains("\"surface.add_agent\""),
        "IPC method `surface.add_agent` disappeared"
    );
    for method in ["surface.park", "surface.show", "surface.close"] {
        assert!(
            handler.contains(&format!("\"{method}\"")),
            "IPC method `{method}` disappeared"
        );
    }
    let rules = read("src-app/src/app/orchestration.rs");
    for field in [
        "\"parked\"",
        "\"shown\"",
        "\"displaced_surface_id\"",
        "\"closed\"",
        "\"stop_turn\"",
    ] {
        assert!(
            rules.contains(field),
            "a move lost its response field {field}"
        );
    }
    for field in [
        "\"surface_id\"",
        "\"thread_id\"",
        "\"agent\"",
        "\"tier\"",
        "\"session_id\"",
        "\"runs_ended\"",
        "\"placement\"",
        "\"placement_reason\"",
        "\"opened_by\"",
    ] {
        assert!(
            rules.contains(field),
            "`surface.add_agent` lost its response field {field}"
        );
    }
    for word in [
        "\"auto\"",
        "\"parked\"",
        "\"pane\"",
        "\"requested\"",
        "\"ceiling\"",
        "\"too_narrow\"",
    ] {
        assert!(
            rules.contains(word),
            "the placement word {word} is published"
        );
    }
    assert!(
        rules.contains("pub(crate) const CODE: i32 = -32004;"),
        "a refusal travels under JSON-RPC code -32004"
    );
    assert!(
        rules.contains("\"data\": { \"reason\": self.word() }"),
        "a refusal names its reason under `error.data.reason`"
    );
    assert!(
        handler.contains("\"rail_status_at_send\""),
        "`surface.send_text` says what the rail said when the text went in"
    );
    for word in [
        "\"not_from_a_pane\"",
        "\"worker_ceiling\"",
        "\"opened_session_cannot_open\"",
        "\"no_room\"",
        "\"not_yours\"",
        "\"self\"",
        "\"waiting\"",
        "\"other_project\"",
        "\"turn_in_flight\"",
        "\"no_interrupt\"",
        "\"unseen_by_person\"",
        "\"person_declined\"",
    ] {
        assert!(rules.contains(word), "the refusal word {word} is published");
    }
    // Stopping a turn: the method, how the stop is reported to the caller,
    // and the word the rail keeps for a run that ended that way.
    assert!(
        handler.contains("\"surface.interrupt\""),
        "IPC method `surface.interrupt` disappeared"
    );
    let verbs = read("src-app/src/cli/session_cmds.rs");
    for word in ["\"interrupted\"", "\"not_running\"", "\"not_confirmed\""] {
        assert!(
            verbs.contains(word),
            "`splitlane interrupt` reports the outcome {word}"
        );
    }
    let rail = read("src-app/src/rail_state.rs");
    for word in ["\"finished\"", "\"failed\"", "\"interrupted\""] {
        assert!(
            rail.contains(word),
            "`rail.last_outcome` lost its word {word}"
        );
    }
    let cli = read("src-app/src/cli/mod.rs");
    assert!(
        cli.contains("pub const EXIT_REFUSED: i32 = 8;"),
        "a refusal is exit code 8"
    );
    // A write into a session a person opened asks the person. That is its
    // own code on the wire and its own exit code, and the answer is read
    // under `drive`.
    assert!(
        rules.contains("pub(crate) const ASKED_PERSON_CODE: i32 = -32005;"),
        "a write that asked a person travels under JSON-RPC code -32005"
    );
    assert!(
        rules.contains("\"data\": { \"reason\": \"asked_person\" }"),
        "a write that asked a person names the reason `asked_person`"
    );
    assert!(
        cli.contains("pub const EXIT_ASKED_PERSON: i32 = 9;"),
        "a write that asked a person is exit code 9"
    );
    assert!(
        cli.contains("pub const EXIT_INTERRUPTED: i32 = 10;"),
        "a wait that ended on a stopped turn is exit code 10"
    );
    assert!(
        read("src-app/src/cli/wait_rail.rs").contains("Outcome::Interrupted => \"interrupted\","),
        "`wait --until turn-end` names a stopped turn `interrupted`"
    );
    assert!(
        handler.matches("value[\"driven_by\"]").count() >= 2,
        "`surface.list` and `surface.status` both say which session drives one"
    );
    for field in ["value[\"drive\"]", "value[\"driven_by\"]"] {
        assert!(
            handler.contains(field),
            "`surface.status` lost its field {field}"
        );
    }
    let drive = read("src-app/src/app/drive.rs");
    for word in [
        "\"asked\"",
        "\"allowed\"",
        "\"declined\"",
        "\"not_offered\"",
    ] {
        assert!(drive.contains(word), "the `drive` word {word} is published");
    }
    assert!(
        drive.contains("wants to send messages to"),
        "the text of a question under `rail.message` is published"
    );
    // Which session opened which is written to `session.json` under this key.
    let schema = read("crates/splitlane-config/src/schema.rs");
    assert!(
        schema.contains("pub opened_by: Option<OpenedBy>"),
        "the session file records who opened a session under `opened_by`"
    );
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
