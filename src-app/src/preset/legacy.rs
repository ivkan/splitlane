//! Reading the presets that used to live in `splitlane.json`.
//!
//! Until this release a preset was a `commands[]` entry carrying a
//! `workspace` object: a name, a cwd, a `layout_preset` string and a
//! `LayoutNode` tree whose leaves were [`SurfaceDefinition`]s. The settings
//! editor was the only thing in the application that read or wrote it.
//!
//! This module is the one-way door out. It reads that shape into
//! [`Preset`] - nothing here writes it back - and it is used twice: by the
//! settings tab while the old store is still the live one, and by the
//! one-time migration that copies the entries into preset files.
//!
//! Two mismatches are worth naming, because they are the reason the old
//! shape had to go:
//!
//! - **The tree was never a tree.** The editor emitted one split with N
//!   leaves; nothing produced nesting. So the leaves are collected in order
//!   and the arrangement is read off `layout_preset`, which is all the
//!   builder ever used.
//! - **`SurfaceDefinition` mixes a template with live state.** `scrollback`,
//!   `font_size` and `surface_id` describe a running surface and have no
//!   meaning in a recipe. They are dropped here rather than carried, which
//!   is also what the writer's golden test enforces on the way out.

use splitlane_config::schema::{LayoutNode, SurfaceDefinition, WorkspaceDefinition};

use super::{PanePreset, Preset, PresetLayout};

/// Read a `commands[].workspace` object as a [`Preset`].
///
/// `fallback_name` is the command entry's own name, used when the workspace
/// object carries none - the settings list showed one or the other, and a
/// migrated file must not lose the label the user saw.
pub fn preset_from_workspace_definition(
    workspace: &WorkspaceDefinition,
    fallback_name: &str,
) -> Preset {
    let name = workspace
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback_name);
    Preset {
        name: Some(name.to_string()),
        // An unrecognised `layout_preset` - including the two arrangements
        // this build no longer draws - reads as the default. The old server
        // folded them into `even_h` anyway, so this loses nothing that was
        // ever honoured.
        layout: workspace
            .layout_preset
            .as_deref()
            .and_then(PresetLayout::from_str)
            .unwrap_or_default(),
        cwd: trimmed(workspace.cwd.as_deref()),
        color: trimmed(workspace.color.as_deref()),
        port_base: None,
        panes: workspace
            .layout
            .as_ref()
            .map(|layout| {
                let mut surfaces = Vec::new();
                collect_surfaces(layout, &mut surfaces);
                surfaces.iter().map(pane_from_surface).collect()
            })
            .unwrap_or_default(),
    }
}

fn pane_from_surface(surface: &SurfaceDefinition) -> PanePreset {
    PanePreset {
        cwd: trimmed(surface.cwd.as_deref()),
        agent: trimmed(surface.agent.as_deref()),
        command: trimmed(surface.command.as_deref()),
        prompt: trimmed(surface.prompt.as_deref()),
        focus: surface.focus,
        env: surface.env.clone().filter(|env| !env.is_empty()),
        // The editor wrote either; `custom_name` is what a rename left
        // behind, and it is the more recent of the two when both exist.
        name: trimmed(surface.custom_name.as_deref()).or_else(|| trimmed(surface.name.as_deref())),
        ..Default::default()
    }
}

fn collect_surfaces(node: &LayoutNode, out: &mut Vec<SurfaceDefinition>) {
    match node {
        LayoutNode::Pane { surfaces } => {
            if surfaces.is_empty() {
                out.push(Default::default());
            } else {
                out.extend(surfaces.iter().cloned());
            }
        }
        LayoutNode::Split { children, .. } => {
            for child in children {
                collect_surfaces(child, out);
            }
        }
    }
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

    fn surface(name: &str, agent: Option<&str>) -> SurfaceDefinition {
        SurfaceDefinition {
            name: Some(name.to_string()),
            agent: agent.map(str::to_string),
            cwd: Some("/dev/backend".to_string()),
            // Live state that must not survive the trip.
            scrollback: Some("x".repeat(4096)),
            font_size: Some(13.0),
            surface_id: Some(7),
            ..Default::default()
        }
    }

    #[test]
    fn a_flat_split_becomes_panes_in_order() {
        let ws = WorkspaceDefinition {
            name: Some("Backend + frontend".to_string()),
            cwd: Some("/work/atlas".to_string()),
            layout_preset: Some("even_v".to_string()),
            color: Some("ff6600".to_string()),
            layout: Some(LayoutNode::Split {
                direction: "vertical".to_string(),
                ratio: None,
                ratios: None,
                children: vec![
                    LayoutNode::Pane {
                        surfaces: vec![surface("api", Some("claude_code"))],
                    },
                    LayoutNode::Pane {
                        surfaces: vec![surface("web", Some("codex"))],
                    },
                ],
            }),
        };
        let preset = preset_from_workspace_definition(&ws, "unused");
        assert_eq!(preset.name.as_deref(), Some("Backend + frontend"));
        assert_eq!(preset.layout, PresetLayout::EvenV);
        assert_eq!(preset.cwd.as_deref(), Some("/work/atlas"));
        assert_eq!(preset.color.as_deref(), Some("ff6600"));
        assert_eq!(preset.panes.len(), 2);
        assert_eq!(preset.panes[0].agent.as_deref(), Some("claude_code"));
        assert_eq!(preset.panes[1].name.as_deref(), Some("web"));
    }

    #[test]
    fn the_commands_entry_name_is_the_fallback() {
        let ws = WorkspaceDefinition {
            name: Some("   ".to_string()),
            cwd: None,
            layout_preset: None,
            color: None,
            layout: None,
        };
        let preset = preset_from_workspace_definition(&ws, "Full stack");
        assert_eq!(preset.name.as_deref(), Some("Full stack"));
        assert_eq!(preset.layout, PresetLayout::EvenH);
    }

    #[test]
    fn the_two_dropped_arrangements_read_as_the_default() {
        for raw in ["main_vertical", "tiled", "nonsense"] {
            let ws = WorkspaceDefinition {
                name: Some("x".to_string()),
                cwd: None,
                layout_preset: Some(raw.to_string()),
                color: None,
                layout: None,
            };
            assert_eq!(
                preset_from_workspace_definition(&ws, "x").layout,
                PresetLayout::EvenH,
                "{raw}"
            );
        }
    }
}
