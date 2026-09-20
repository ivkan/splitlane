//! A preset: a saved set of panes, each carrying its own directory.
//!
//! One type, and one builder for the `workspace.up` request both ways of
//! launching a preset send. Before this module there were two of each: the
//! `splitlane.workspace.toml` the CLI read, and the `commands[].workspace`
//! object the settings editor wrote - two schemas that met only on the wire,
//! each with its own validation and its own rule for a pane's default
//! directory. That is where the drift lived.
//!
//! # What the union actually is
//!
//! Less than the two shapes suggest. `WorkspaceDefinition` carried a freely
//! recursive `LayoutNode` where the CLI had a four-value enum, so the GUI
//! schema looked strictly richer - but the GUI's *builder* never wrote a
//! nested tree: it emitted one split with N leaves. There is no recursion in
//! a preset, only an arrangement, so the union is
//! `name`, `layout`, `cwd`, `color`, `panes[…]` and `port_base`. `color` is
//! the one honestly GUI-only field, and it is here on purpose.
//!
//! # Three arrangements
//!
//! `main_vertical` and `tiled` both parsed and neither did anything:
//! `build_up_layout` folds every name it does not know into `even_h`, so
//! a file asking for an arbitrary grid was accepted by the parser and silently
//! ignored by the server. A container holds a row or a column of at most four
//! panes, or a grid of exactly four; those are the three values. The **wire**
//! still accepts the old names (`workspace.up` is an external contract), but
//! nothing in this app writes them any more, and a preset file that names one
//! now says so.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use splitlane_config::schema::SplitlaneConfig;

use crate::agent_launcher::TerminalAgent;
use crate::layout::MAX_PANES;

pub mod legacy;
pub mod store;

/// How a preset's panes are arranged.
///
/// Three values, one per named form. Serialized as `even_h` /
/// `even_v` / `grid`, the names `workspace.up` uses - `snake_case` maps the
/// variants onto exactly those, so [`PresetLayout::as_ipc`] and the derived
/// serde spelling cannot drift apart.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetLayout {
    /// Side by side.
    #[default]
    EvenH,
    /// Stacked.
    EvenV,
    /// Two rows of two - the third named form, added because four panes in a
    /// row do not fit a laptop screen. Built only from exactly
    /// four panes; a preset carrying this word with any other number falls back
    /// to side by side, because a grid has four cells and a preset cannot
    /// invent the panes to fill them.
    Grid,
}

impl PresetLayout {
    /// The `layout` string the `workspace.up` IPC method expects.
    pub fn as_ipc(self) -> &'static str {
        match self {
            PresetLayout::EvenH => "even_h",
            PresetLayout::EvenV => "even_v",
            PresetLayout::Grid => "grid",
        }
    }

    /// Parse a persisted or wire value. `None` for anything else, including
    /// the two arrangements this build no longer draws.
    pub fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "even_h" => Some(PresetLayout::EvenH),
            "even_v" => Some(PresetLayout::EvenV),
            "grid" => Some(PresetLayout::Grid),
            _ => None,
        }
    }
}

/// One pane of a preset.
///
/// A template, not a live surface: nothing here describes what a pane
/// currently *holds*. That separation is the point of the type - see the
/// golden test in [`crate::preset::store`] which holds the writer to it.
///
/// `deny_unknown_fields` survives here even though the *preset reader* is
/// deliberately tolerant, because the two are not in conflict: the reader
/// strips (and warns about) keys it does not know before it hands the table
/// to serde, so a typo is reported once, in the reader's words, rather than
/// as a serde error - while the flow spec, which nothing rewrites, keeps the
/// hard refusal it always had.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PanePreset {
    /// Working directory. Blank means the project the preset was saved from
    /// ([`Preset::cwd`]); `~` is expanded when the pane is launched.
    pub cwd: Option<String>,
    /// Agent to launch (claude / codex / opencode / gemini / …). Mutually
    /// exclusive with [`Self::command`].
    pub agent: Option<String>,
    /// Raw command to run instead of an agent.
    pub command: Option<String>,
    /// Prompt pre-filled into the agent's input box. Never auto-submitted.
    pub prompt: Option<String>,
    /// Whether this pane is the focused one.
    pub focus: Option<bool>,
    /// Per-pane env overrides, merged over the global `terminal.env`. Values
    /// may reference `${port_offset}`.
    pub env: Option<HashMap<String, String>>,
    /// Optional pane name.
    pub name: Option<String>,
    /// Branch to isolate this pane on via a git worktree. Requires
    /// [`Self::cwd`] (inside a git repo); the pane spawns in
    /// `<repo>.worktrees/<branch-slug>` instead.
    pub worktree: Option<String>,
    /// Copy the repo's top-level gitignored `.env*` files into the worktree.
    /// Default true; only meaningful with [`Self::worktree`].
    pub copy_env: Option<bool>,
    /// Command run inside a freshly created worktree before the agent spawns.
    /// Failure warns but never blocks.
    pub setup: Option<String>,
    /// Wall-clock bound for [`Self::setup`], seconds (default 300).
    pub setup_timeout_secs: Option<u64>,
    /// Worktree teardown at close: `"auto"` (default) or `"keep"`.
    pub worktree_teardown: Option<String>,
}

/// A saved set of panes.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preset {
    /// Display name. A file-backed preset takes its file stem when blank.
    pub name: Option<String>,
    /// How the panes are arranged.
    pub layout: PresetLayout,
    /// The project the preset was saved from - the default directory for a
    /// pane that names none.
    pub cwd: Option<String>,
    /// Accent as a 6-digit hex string. GUI-only: `splitlane up` reads it and
    /// does nothing with it, which is better than two files disagreeing.
    pub color: Option<String>,
    /// Base port for `${port_offset}` allocation. Default 3000.
    pub port_base: Option<u16>,
    pub panes: Vec<PanePreset>,
}

impl Preset {
    /// The name a reader sees.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Untitled preset")
    }

    /// The directories this preset spans, in pane order, deduplicated - the
    /// Launch pad row's second mono line. A pane with no directory of its own
    /// contributes the preset's.
    pub fn directories(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for pane in &self.panes {
            let dir = pane
                .cwd
                .as_deref()
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
                .or_else(|| {
                    self.cwd
                        .as_deref()
                        .map(str::trim)
                        .filter(|cwd| !cwd.is_empty())
                });
            if let Some(dir) = dir
                && !out.iter().any(|seen| seen == dir)
            {
                out.push(dir.to_string());
            }
        }
        out
    }

    /// Semantic invariants, checked after parsing.
    ///
    /// These are orthogonal to how tolerant the *reader* is about unknown
    /// keys (see [`crate::preset::store`]): a misspelled key is a warning
    /// because a file written by a future release must still run, while
    /// `agent` and `command` on one pane is a contradiction no release can
    /// resolve.
    pub fn validate(&self) -> Result<(), String> {
        if self.panes.is_empty() {
            return Err("preset has no [[panes]]".to_string());
        }
        if self.panes.len() > MAX_PANES {
            return Err(format!(
                "too many panes ({} > {MAX_PANES})",
                self.panes.len()
            ));
        }
        for (i, pane) in self.panes.iter().enumerate() {
            validate_pane(i, pane)?;
        }
        Ok(())
    }
}

/// Per-pane invariants: `agent` XOR `command`, plus the worktree rules.
pub fn validate_pane(i: usize, pane: &PanePreset) -> Result<(), String> {
    if pane.agent.is_some() && pane.command.is_some() {
        return Err(format!(
            "pane {i}: set either `agent` or `command`, not both"
        ));
    }
    validate_worktree_fields(i, pane)
}

/// Worktree-field invariants. `worktree` needs a `cwd` to locate the repo;
/// the companion fields are inert without it, so their presence alone is a
/// mistake worth refusing. A leading `-` in the branch would read as a git
/// flag (CWE-88) - refused here rather than trusted to downstream quoting.
fn validate_worktree_fields(i: usize, pane: &PanePreset) -> Result<(), String> {
    match pane.worktree.as_deref() {
        Some(branch) => {
            if branch.is_empty() {
                return Err(format!("pane {i}: `worktree` must name a branch"));
            }
            if branch.starts_with('-') {
                return Err(format!(
                    "pane {i}: branch '{branch}' must not start with '-'"
                ));
            }
            // A branch whose filesystem slug is empty (dot-only: `.`, `..`)
            // has no safe directory name - `..` would be a traversal
            // component of the worktree path.
            if crate::workspace::worktree::branch_slug(branch).is_empty() {
                return Err(format!(
                    "pane {i}: branch '{branch}' has no filesystem-safe name \
                     (dot-only names are not allowed)"
                ));
            }
            if pane.cwd.is_none() {
                return Err(format!(
                    "pane {i}: `worktree` requires `cwd` (to locate the git repository)"
                ));
            }
        }
        None => {
            for (field, set) in [
                ("copy_env", pane.copy_env.is_some()),
                ("setup", pane.setup.is_some()),
                ("setup_timeout_secs", pane.setup_timeout_secs.is_some()),
                ("worktree_teardown", pane.worktree_teardown.is_some()),
            ] {
                if set {
                    return Err(format!("pane {i}: `{field}` requires `worktree`"));
                }
            }
        }
    }
    if let Some(policy) = pane.worktree_teardown.as_deref()
        && !matches!(policy, "auto" | "keep")
    {
        return Err(format!(
            "pane {i}: `worktree_teardown` must be \"auto\" or \"keep\", got '{policy}'"
        ));
    }
    Ok(())
}

/// Per-pane facts only one of the two launch paths computes.
///
/// `splitlane up` plans worktrees and allocates port strides before it builds
/// the request; the GUI does neither. Rather than teach the builder both
/// jobs, each caller hands in what it resolved and the builder stays the one
/// place that decides the *shape* of the request.
#[derive(Debug, Clone, Default)]
pub struct PaneOverride {
    /// Replaces the pane's own cwd - the worktree path, once created.
    pub cwd: Option<String>,
    /// Replaces the pane's env - the same map with `${port_offset}`
    /// substituted.
    pub env: Option<HashMap<String, String>>,
    /// The `managed_worktree` block the server records for teardown.
    pub managed_worktree: Option<Value>,
}

/// Build the `workspace.up` request for `preset`.
///
/// `overrides` is indexed by pane; a short slice simply leaves the rest
/// unoverridden. Both launch paths call this, which is the whole point:
/// the default-cwd rule, the agent-to-command resolution and the pane
/// profile are decided once.
pub fn workspace_up_params(
    preset: &Preset,
    config: &SplitlaneConfig,
    overrides: &[PaneOverride],
) -> Result<Value, String> {
    preset.validate()?;

    let project_cwd = preset
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty());

    let mut panes = Vec::with_capacity(preset.panes.len());
    for (idx, pane) in preset.panes.iter().enumerate() {
        let empty = PaneOverride::default();
        let over = overrides.get(idx).unwrap_or(&empty);
        // The env this pane will be handed, resolved before it is checked so
        // that the refusal below reads the very map the request carries
        // rather than a second copy of the same rule.
        let env = over.env.clone().or_else(|| pane.env.clone());
        refuse_unserviced_pane(idx, pane, over, env.as_ref())?;

        // A pane with no directory of its own inherits the preset's - "blank
        // means the project the preset was saved from". One rule, where the
        // two builders used to disagree: the CLI left it to the server and
        // the GUI substituted the project path.
        let cwd = over.cwd.clone().or_else(|| {
            pane.cwd
                .as_deref()
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
                .map(str::to_string)
                .or_else(|| project_cwd.map(str::to_string))
        });

        let (command, profile) = resolve_pane_command(idx, pane, config)?;

        let mut spec = serde_json::Map::new();
        spec.insert("cwd".to_string(), json!(cwd));
        spec.insert("command".to_string(), json!(command));
        spec.insert("prompt".to_string(), json!(trimmed(pane.prompt.as_deref())));
        spec.insert("focus".to_string(), json!(pane.focus));
        spec.insert("env".to_string(), json!(env));
        spec.insert("name".to_string(), json!(trimmed(pane.name.as_deref())));
        spec.insert("profile".to_string(), json!(profile));
        spec.insert("managed_worktree".to_string(), json!(over.managed_worktree));
        panes.push(Value::Object(spec));
    }

    Ok(json!({
        "name": trimmed(preset.name.as_deref()),
        "layout": preset.layout.as_ipc(),
        "panes": panes,
    }))
}

/// The one token a pane's `env` may reference, spelled once.
pub const PORT_OFFSET_TOKEN: &str = "${port_offset}";

/// Refuse a pane that asks for something only `splitlane up` performs.
///
/// Two of a pane's fields are honoured by exactly one of the two launch
/// paths: a `worktree` has to be planned and created, and `${port_offset}`
/// has to be allocated a free stride. The doc comment on [`PaneOverride`]
/// has said so from the beginning - "the GUI does neither" - and until this
/// check both simply disappeared on that path: a pane asking for
/// `worktree = "feat/x"` spawned in the main checkout beside every other
/// pane of the preset, and `${port_offset}` reached the terminal as that
/// literal string.
///
/// The refusal lives **here**, in the builder both paths call, and not at
/// the GUI call sites. That is the whole of the choice: a check in the GUI
/// closes the two doors that exist today and is silent for the third one
/// somebody adds, while a check here makes the defect structurally
/// impossible - a caller that does not resolve these fields cannot build a
/// request at all.
///
/// It refuses **always**, including the case where the branch already has a
/// worktree this app could reuse. Deciding that is the same planning the GUI
/// does not do, and honouring half the cases would smear the meaning of the
/// field: `worktree` either puts the pane somewhere else or it is refused.
///
/// **Two fields, not six.** `copy_env`, `setup`, `setup_timeout_secs` and
/// `worktree_teardown` are equally CLI-only, and each is already refused by
/// [`validate_worktree_fields`] unless the pane also names a `worktree` - so
/// the `worktree` arm below covers all five by construction rather than by a
/// list somebody has to keep in step. And `${port_offset}` is an **env**
/// feature by schema: the CLI substitutes it in `env` values and nowhere
/// else, so a token written into `command` is a literal string on both paths
/// and not a place the two disagree.
///
/// `env` is handed in already resolved - the map the request will carry -
/// rather than resolved a second time here, so the check cannot come to read
/// something other than what ships.
///
/// The sentence is what the person reads - it names the pane and the way
/// out, not the builder's insides.
fn refuse_unserviced_pane(
    idx: usize,
    pane: &PanePreset,
    over: &PaneOverride,
    env: Option<&HashMap<String, String>>,
) -> Result<(), String> {
    if let Some(branch) = pane.worktree.as_deref()
        && over.managed_worktree.is_none()
    {
        return Err(format!(
            "pane {idx}: nothing here creates the worktree for `{branch}` - run it with \
             'splitlane up', or drop the `worktree` field to start in the project itself"
        ));
    }
    if let Some(env) = env {
        let mut keys: Vec<&str> = env
            .iter()
            .filter(|(_, value)| value.contains(PORT_OFFSET_TOKEN))
            .map(|(key, _)| key.as_str())
            .collect();
        // Sorted and all of them: one broken preset gives one sentence,
        // the same one every run over a hash map, and fixing the variable it
        // names does not reveal a second one on the next attempt.
        keys.sort_unstable();
        if !keys.is_empty() {
            return Err(format!(
                "pane {idx}: env `{}` still reads {PORT_OFFSET_TOKEN} and nothing here \
                 allocates a port - run it with 'splitlane up', or write the port you \
                 want",
                keys.join("`, `")
            ));
        }
    }
    Ok(())
}

/// A pane's launch command and its terminal profile.
///
/// An `agent` resolves to that CLI's launch command and marks the pane as an
/// agent surface; the binary is verified on PATH so the whole preset fails
/// before any pane spawns. A raw `command` passes through. Neither leaves a
/// bare shell, which is the third, legitimate case.
fn resolve_pane_command(
    idx: usize,
    pane: &PanePreset,
    config: &SplitlaneConfig,
) -> Result<(Option<String>, &'static str), String> {
    let Some(tag) = pane
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    else {
        return Ok((trimmed(pane.command.as_deref()), "normal"));
    };
    let agent = resolve_agent(tag).ok_or_else(|| format!("pane {idx}: unknown agent '{tag}'"))?;
    if !agent.is_installed() {
        return Err(format!(
            "pane {idx}: agent '{tag}' ({}) not found on PATH",
            agent.binary()
        ));
    }
    Ok((Some(agent.launch_command(config)), "agent"))
}

/// Map a friendly agent name to a [`TerminalAgent`]. Accepts hyphen or
/// underscore separators and the bare `claude` alias for Claude Code.
pub fn resolve_agent(name: &str) -> Option<TerminalAgent> {
    let normalized = name.trim().to_lowercase().replace('-', "_");
    let tag = match normalized.as_str() {
        "claude" => "claude_code",
        other => other,
    };
    TerminalAgent::from_tag(tag)
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(agent: Option<&str>, command: Option<&str>) -> PanePreset {
        PanePreset {
            agent: agent.map(str::to_string),
            command: command.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn layout_has_two_values_and_round_trips_the_wire_names() {
        assert_eq!(PresetLayout::from_str("even_h"), Some(PresetLayout::EvenH));
        assert_eq!(PresetLayout::from_str("even_v"), Some(PresetLayout::EvenV));
        // The two arrangements the server folded into `even_h` anyway. They
        // are no longer values a preset can hold, so a file naming one is
        // told rather than quietly redrawn.
        assert_eq!(PresetLayout::from_str("main_vertical"), None);
        assert_eq!(PresetLayout::from_str("tiled"), None);
        assert_eq!(PresetLayout::default().as_ipc(), "even_h");
    }

    #[test]
    fn a_pane_without_a_directory_inherits_the_presets() {
        let preset = Preset {
            cwd: Some("/work/atlas".to_string()),
            panes: vec![
                PanePreset {
                    cwd: Some("/dev/frontend".to_string()),
                    ..Default::default()
                },
                PanePreset::default(),
            ],
            ..Default::default()
        };
        let config = SplitlaneConfig::default();
        let params = workspace_up_params(&preset, &config, &[]).expect("params");
        let panes = params["panes"].as_array().expect("panes");
        assert_eq!(panes[0]["cwd"], json!("/dev/frontend"));
        assert_eq!(panes[1]["cwd"], json!("/work/atlas"));
    }

    #[test]
    fn an_override_wins_over_both() {
        let preset = Preset {
            cwd: Some("/work/atlas".to_string()),
            panes: vec![PanePreset {
                cwd: Some("/dev/frontend".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let over = PaneOverride {
            cwd: Some("/work/atlas.worktrees/feat-x".to_string()),
            ..Default::default()
        };
        let params = workspace_up_params(
            &preset,
            &SplitlaneConfig::default(),
            std::slice::from_ref(&over),
        )
        .expect("params");
        assert_eq!(
            params["panes"][0]["cwd"],
            json!("/work/atlas.worktrees/feat-x")
        );
    }

    fn worktree_pane() -> Preset {
        Preset {
            panes: vec![PanePreset {
                cwd: Some("/repo".to_string()),
                command: Some("pnpm dev".to_string()),
                worktree: Some("feat/x".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_worktree_pane_is_refused_when_nobody_created_the_worktree() {
        // The GUI path: empty overrides. Before this the pane spawned in the
        // main checkout and said nothing.
        let err =
            workspace_up_params(&worktree_pane(), &SplitlaneConfig::default(), &[]).unwrap_err();
        assert!(err.contains("pane 0"), "names the pane: {err}");
        assert!(err.contains("feat/x"), "names the branch: {err}");
        assert!(err.contains("splitlane up"), "names the way out: {err}");
    }

    #[test]
    fn a_worktree_pane_passes_once_the_worktree_is_in_the_override() {
        // The CLI path, both of its cases: `plan_worktree` returns a plan
        // whenever the field is set - `action: "reuse"` when the worktree
        // already exists - so `managed_worktree` is always present there.
        for action in ["create", "reuse"] {
            let over = PaneOverride {
                cwd: Some("/repo.worktrees/feat-x".to_string()),
                managed_worktree: Some(json!({
                    "path": "/repo.worktrees/feat-x",
                    "repo_root": "/repo",
                    "branch": "feat/x",
                    "teardown": "auto",
                    "action": action,
                })),
                ..Default::default()
            };
            let params = workspace_up_params(
                &worktree_pane(),
                &SplitlaneConfig::default(),
                std::slice::from_ref(&over),
            )
            .unwrap_or_else(|err| panic!("{action}: {err}"));
            assert_eq!(params["panes"][0]["cwd"], json!("/repo.worktrees/feat-x"));
        }
    }

    #[test]
    fn a_blank_worktree_is_still_refused_by_validate_and_never_reaches_this_check() {
        // Where the two refusals meet. A whitespace branch has no
        // filesystem-safe slug, so `validate` refuses it by name before the
        // builder gets to ask who created the worktree - which is why the
        // Launch pad may treat a blank field as no field without leaving
        // anybody refused over something they cannot see.
        let mut preset = worktree_pane();
        preset.panes[0].worktree = Some("  ".to_string());
        let err = workspace_up_params(&preset, &SplitlaneConfig::default(), &[]).unwrap_err();
        assert!(
            err.contains("filesystem-safe"),
            "validate answers first: {err}"
        );
    }

    fn port_pane() -> Preset {
        let mut env = HashMap::new();
        env.insert("API_PORT".to_string(), PORT_OFFSET_TOKEN.to_string());
        env.insert("QUIET".to_string(), "1".to_string());
        Preset {
            panes: vec![PanePreset {
                command: Some("pnpm dev".to_string()),
                env: Some(env),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn an_unallocated_port_token_is_refused_rather_than_passed_through() {
        // The second instance of the same defect: without an allocation the
        // token used to reach the terminal as that literal string.
        let mut preset = port_pane();
        preset.panes[0]
            .env
            .as_mut()
            .expect("env")
            .insert("WS_PORT".to_string(), "ws://:${port_offset}".to_string());
        let err = workspace_up_params(&preset, &SplitlaneConfig::default(), &[]).unwrap_err();
        assert!(err.contains("pane 0"), "names the pane: {err}");
        // Every variable, so fixing the one it names does not reveal the next.
        assert!(err.contains("API_PORT"), "names the variable: {err}");
        assert!(err.contains("WS_PORT"), "names the second one too: {err}");
        assert!(err.contains("splitlane up"), "names the way out: {err}");
    }

    #[test]
    fn a_substituted_env_passes_and_an_unrelated_one_is_untouched() {
        let mut substituted = HashMap::new();
        substituted.insert("API_PORT".to_string(), "3010".to_string());
        substituted.insert("QUIET".to_string(), "1".to_string());
        let over = PaneOverride {
            env: Some(substituted),
            ..Default::default()
        };
        let params = workspace_up_params(
            &port_pane(),
            &SplitlaneConfig::default(),
            std::slice::from_ref(&over),
        )
        .expect("params");
        assert_eq!(params["panes"][0]["env"]["API_PORT"], json!("3010"));
    }

    #[test]
    fn a_raw_command_pane_is_not_an_agent_surface() {
        let preset = Preset {
            panes: vec![pane(None, Some("pnpm dev"))],
            ..Default::default()
        };
        let params =
            workspace_up_params(&preset, &SplitlaneConfig::default(), &[]).expect("params");
        assert_eq!(params["panes"][0]["command"], json!("pnpm dev"));
        assert_eq!(params["panes"][0]["profile"], json!("normal"));
    }

    #[test]
    fn agent_and_command_together_is_refused() {
        let preset = Preset {
            panes: vec![pane(Some("claude"), Some("vim"))],
            ..Default::default()
        };
        let err = preset.validate().unwrap_err();
        assert!(err.contains("either"), "got: {err}");
    }

    #[test]
    fn an_unknown_agent_is_refused_before_anything_spawns() {
        let preset = Preset {
            panes: vec![pane(Some("clod"), None)],
            ..Default::default()
        };
        let err = workspace_up_params(&preset, &SplitlaneConfig::default(), &[]).unwrap_err();
        assert!(err.contains("unknown agent"), "got: {err}");
    }

    #[test]
    fn resolve_agent_accepts_aliases() {
        assert_eq!(resolve_agent("claude"), Some(TerminalAgent::ClaudeCode));
        assert_eq!(
            resolve_agent("claude-code"),
            Some(TerminalAgent::ClaudeCode)
        );
        assert_eq!(resolve_agent("Codex"), Some(TerminalAgent::Codex));
        assert_eq!(resolve_agent("nope"), None);
    }

    #[test]
    fn empty_and_oversized_pane_lists_are_refused() {
        let empty = Preset::default();
        assert!(empty.validate().unwrap_err().contains("no [[panes]]"));
        let too_many = Preset {
            panes: vec![PanePreset::default(); MAX_PANES + 1],
            ..Default::default()
        };
        assert!(too_many.validate().unwrap_err().contains("too many panes"));
    }

    #[test]
    fn directories_lists_each_once_in_pane_order() {
        let preset = Preset {
            cwd: Some("/work/atlas".to_string()),
            panes: vec![
                PanePreset {
                    cwd: Some("/dev/backend".to_string()),
                    ..Default::default()
                },
                PanePreset {
                    cwd: Some("/dev/frontend".to_string()),
                    ..Default::default()
                },
                PanePreset {
                    cwd: Some("/dev/backend".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(preset.directories(), ["/dev/backend", "/dev/frontend"]);
    }

    #[test]
    fn a_pane_with_no_directory_of_its_own_shows_the_presets() {
        let preset = Preset {
            cwd: Some("/work/atlas".to_string()),
            panes: vec![PanePreset::default(), PanePreset::default()],
            ..Default::default()
        };
        assert_eq!(preset.directories(), ["/work/atlas"]);
    }

    #[test]
    fn worktree_rules_survive_the_move() {
        let mut p = pane(Some("claude"), None);
        p.worktree = Some("feat/x".to_string());
        let preset = Preset {
            panes: vec![p],
            ..Default::default()
        };
        assert!(preset.validate().unwrap_err().contains("requires `cwd`"));

        let mut p = pane(Some("claude"), None);
        p.cwd = Some("/tmp".to_string());
        p.worktree = Some("--force".to_string());
        let preset = Preset {
            panes: vec![p],
            ..Default::default()
        };
        assert!(
            preset
                .validate()
                .unwrap_err()
                .contains("must not start with '-'")
        );

        let mut p = pane(Some("claude"), None);
        p.setup = Some("bun install".to_string());
        let preset = Preset {
            panes: vec![p],
            ..Default::default()
        };
        assert!(
            preset
                .validate()
                .unwrap_err()
                .contains("`setup` requires `worktree`")
        );
    }

    /// The one thing this enum must never do: spell a value differently
    /// depending on which door it goes out of. `as_ipc` writes the TOML and
    /// speaks to `workspace.up`; the derived serde spelling reads the CLI's own
    /// spec file. A variant added to one and not the other is a preset that
    /// saves and does not load.
    #[test]
    fn every_form_has_one_spelling_on_both_doors() {
        for form in [PresetLayout::EvenH, PresetLayout::EvenV, PresetLayout::Grid] {
            let wire = form.as_ipc();
            assert_eq!(PresetLayout::from_str(wire), Some(form), "as_ipc/from_str");
            let json = serde_json::to_string(&form).expect("serializable");
            assert_eq!(json, format!("\"{wire}\""), "serde spelling");
        }
        // And the two arrangements this build does not draw are still not
        // values, so a spec asking for one is refused rather than quietly
        // given something else.
        assert_eq!(PresetLayout::from_str("tiled"), None);
        assert_eq!(PresetLayout::from_str("main_vertical"), None);
    }
}
