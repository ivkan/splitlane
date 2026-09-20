//! Declarative workspace spec for `splitlane up`.
//!
//! A `splitlane.workspace.toml` describes one preset: an arrangement plus a
//! list of panes, each with an optional cwd, an agent (or raw command) to run,
//! and a prompt to pre-fill. The *type* it parses into is
//! [`crate::preset::Preset`] - the one shared with the GUI, so the file the
//! Launch pad writes is the file this reads.
//!
//! What stays here is the file-facing half: reading TOML, and the
//! `deny_unknown_fields` strictness that turns a misspelled key in a
//! hand-written spec into an explicit error. Business invariants live on the
//! type ([`Preset::validate`]).
//!
//! Example:
//! ```toml
//! name = "feature-x"
//! layout = "even_h"
//!
//! [[panes]]
//! cwd = "~/dev/backend"
//! agent = "claude"
//! prompt = "review the diff on this branch"
//! focus = true
//!
//! [[panes]]
//! cwd = "~/dev/frontend"
//! agent = "codex"
//! ```

pub use crate::preset::{
    PanePreset as PaneSpec, Preset as WorkspaceSpec, PresetLayout as LayoutPreset, validate_pane,
};

/// Parse + validate a preset from TOML source.
pub fn load(src: &str) -> Result<WorkspaceSpec, String> {
    let spec: WorkspaceSpec = toml::from_str(src).map_err(|e| e.to_string())?;
    spec.validate()?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_spec() {
        let spec = load(
            r#"
            name = "feat-x"
            layout = "even_v"

            [[panes]]
            cwd = "/tmp"
            agent = "claude"
            prompt = "do the thing"
            focus = true

            [[panes]]
            command = "cargo watch"
            "#,
        )
        .expect("valid spec");
        assert_eq!(spec.name.as_deref(), Some("feat-x"));
        assert_eq!(spec.layout, LayoutPreset::EvenV);
        assert_eq!(spec.panes.len(), 2);
        assert_eq!(spec.panes[0].agent.as_deref(), Some("claude"));
        assert_eq!(spec.panes[0].focus, Some(true));
        assert_eq!(spec.panes[1].command.as_deref(), Some("cargo watch"));
    }

    #[test]
    fn the_two_arrangements_the_server_ignored_are_no_longer_values() {
        // `build_up_layout` folded both into `even_h`, so a spec asking for a
        // grid parsed and then did something else. It now says so instead:
        // `deny_unknown_fields` guards a typo in a *key*, and this is the
        // matching guard for a typo in a *value*.
        for layout in ["main_vertical", "tiled"] {
            let err = load(&format!(
                "layout = \"{layout}\"\n[[panes]]\nagent = \"claude\"\n"
            ))
            .unwrap_err();
            assert!(err.contains("even_h") || err.contains(layout), "got: {err}");
        }
    }

    #[test]
    fn layout_defaults_to_even_h() {
        let spec = load("[[panes]]\nagent = \"codex\"\n").expect("valid");
        assert_eq!(spec.layout, LayoutPreset::EvenH);
        assert_eq!(spec.layout.as_ipc(), "even_h");
    }

    #[test]
    fn rejects_unknown_field() {
        let err = load("agnt = \"claude\"\n[[panes]]\nagent = \"claude\"\n").unwrap_err();
        assert!(
            err.contains("agnt") || err.contains("unknown"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_agent_and_command_together() {
        let err = load("[[panes]]\nagent = \"claude\"\ncommand = \"vim\"\n").unwrap_err();
        assert!(err.contains("either"), "got: {err}");
    }

    #[test]
    fn rejects_empty_panes() {
        let err = load("name = \"x\"\n").unwrap_err();
        assert!(err.contains("no [[panes]]"), "got: {err}");
    }

    #[test]
    fn parses_worktree_fields() {
        let spec = load(
            "port_base = 4000\n[[panes]]\ncwd = \"/tmp\"\nagent = \"claude\"\nworktree = \"feat/x\"\ncopy_env = false\nsetup = \"bun install\"\nworktree_teardown = \"keep\"\n",
        )
        .expect("valid");
        assert_eq!(spec.port_base, Some(4000));
        let p = &spec.panes[0];
        assert_eq!(p.worktree.as_deref(), Some("feat/x"));
        assert_eq!(p.copy_env, Some(false));
        assert_eq!(p.setup.as_deref(), Some("bun install"));
        assert_eq!(p.worktree_teardown.as_deref(), Some("keep"));
    }

    #[test]
    fn worktree_requires_cwd() {
        let err = load("[[panes]]\nagent = \"claude\"\nworktree = \"feat/x\"\n").unwrap_err();
        assert!(err.contains("requires `cwd`"), "got: {err}");
    }

    #[test]
    fn worktree_branch_must_not_look_like_a_flag() {
        // CWE-88: a leading '-' would be parsed as a git flag downstream.
        let err = load("[[panes]]\ncwd = \"/tmp\"\nagent = \"claude\"\nworktree = \"--force\"\n")
            .unwrap_err();
        assert!(err.contains("must not start with '-'"), "got: {err}");
    }

    #[test]
    fn worktree_branch_dot_only_is_rejected() {
        // A dot-only branch slug would be a `..` traversal component in the
        // (destructive) worktree path - refused at parse, atomically.
        for branch in ["..", ".", "..."] {
            let err = load(&format!(
                "[[panes]]\ncwd = \"/tmp\"\nagent = \"claude\"\nworktree = \"{branch}\"\n"
            ))
            .unwrap_err();
            assert!(
                err.contains("filesystem-safe"),
                "branch {branch}: got: {err}"
            );
        }
    }

    #[test]
    fn worktree_companion_fields_require_worktree() {
        let err = load("[[panes]]\nagent = \"claude\"\nsetup = \"bun install\"\n").unwrap_err();
        assert!(err.contains("`setup` requires `worktree`"), "got: {err}");
        let err = load("[[panes]]\nagent = \"claude\"\ncopy_env = true\n").unwrap_err();
        assert!(err.contains("`copy_env` requires `worktree`"), "got: {err}");
    }

    #[test]
    fn worktree_teardown_value_is_validated() {
        let err = load(
            "[[panes]]\ncwd = \"/tmp\"\nagent = \"claude\"\nworktree = \"x\"\nworktree_teardown = \"delete\"\n",
        )
        .unwrap_err();
        assert!(err.contains("auto"), "got: {err}");
    }

    #[test]
    fn rejects_too_many_panes() {
        // The post-deserialization validation bounds the pane count
        // to MAX_PANES so a runaway spec can't drive the server past its cap.
        let src = "[[panes]]\nagent = \"claude\"\n".repeat(crate::layout::MAX_PANES + 1);
        let err = load(&src).unwrap_err();
        assert!(err.contains("too many panes"), "got: {err}");
    }
}
