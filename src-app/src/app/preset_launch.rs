//! Running a preset: turning one into the panes of an open project.
//!
//! Lifted out of the settings editor, which is where it used to live and
//! which is being deleted. The code is the same; its home is not, because a
//! preset is launched from the Launch pad now and the settings section is a
//! read-only list that points there.
//!
//! **Which project a preset runs in** is the preset's own `cwd`, matched
//! against the open projects. A preset naming a project that is not open says
//! so rather than running somewhere else - it carries a directory precisely
//! so that it cannot be ambiguous. A preset naming no project runs in the
//! active one, which is what "blank means the project it was saved from"
//! resolves to when it was never saved from one.

use gpui::{AppContext, Context};
use serde_json::Value;

use crate::SplitlaneApp;
use crate::app::ipc_handler::{
    build_up_layout, canonicalize_workspace_cwd, dedupe_planned_pane_labels,
    parse_workspace_pane_plan, stage_planned_pane_env,
};
use crate::layout::MAX_PANES;
use crate::preset::Preset;
use crate::terminal::TerminalView;

impl SplitlaneApp {
    /// The preset saved from the project at `idx`, if the library holds one.
    ///
    /// Matched on the directory, because that is what a preset carries. The
    /// first match wins: two presets for one project is a reasonable thing to
    /// have, and the row is a shortcut, not the list - the Launch pad is.
    pub(crate) fn preset_for_project(&self, idx: usize) -> Option<usize> {
        let project = canonicalize_workspace_cwd(&self.workspaces.get(idx)?.cwd).ok()?;
        self.presets.iter().position(|stored| {
            stored
                .preset
                .cwd
                .as_deref()
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
                .is_some_and(|cwd| workspace_cwd_matches(cwd, &project))
        })
    }

    /// The project row's "Run preset": run it in that project, whatever the
    /// preset's own directory resolves to.
    pub(crate) fn run_preset_for_project(
        &mut self,
        stored: Option<crate::preset::store::StoredPreset>,
        target_idx: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = stored else {
            return;
        };
        let name = stored.preset.display_name().to_string();
        let result = crate::preset::workspace_up_params(&stored.preset, &self.cached_config, &[])
            .and_then(|params| {
                self.launch_workspace_params_in_open_workspace(&params, target_idx, cx)
            });
        match result {
            Ok(()) => self.show_toast(format!("{name} started"), cx),
            Err(message) => self.show_toast(format!("{name}: {message}"), cx),
        }
    }

    /// Run `preset`, replacing the target project's panes.
    ///
    /// The error is a sentence, not a code: it goes straight into the Launch
    /// pad's status line, where the user is looking.
    pub(crate) fn run_preset(
        &mut self,
        preset: &Preset,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let params = crate::preset::workspace_up_params(preset, &self.cached_config, &[])?;
        let target_idx = match preset
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|cwd| !cwd.is_empty())
        {
            Some(project) => self
                .open_workspace_index_for_project(project)
                .ok_or_else(|| format!("open {project} first"))?,
            None => self.active_idx,
        };
        self.launch_workspace_params_in_open_workspace(&params, target_idx, cx)
    }

    pub(crate) fn open_workspace_index_for_project(&self, project: &str) -> Option<usize> {
        let project = canonicalize_workspace_cwd(project).ok()?;
        if let Some(active) = self.workspaces.get(self.active_idx)
            && workspace_cwd_matches(&active.cwd, &project)
        {
            return Some(self.active_idx);
        }
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(idx, workspace)| {
                workspace_cwd_matches(&workspace.cwd, &project).then_some(idx)
            })
    }

    pub(crate) fn launch_workspace_params_in_open_workspace(
        &mut self,
        params: &Value,
        target_idx: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let preset = params
            .get("layout")
            .and_then(Value::as_str)
            .unwrap_or("even_h");
        let pane_specs = params
            .get("panes")
            .and_then(Value::as_array)
            .filter(|panes| !panes.is_empty())
            .ok_or_else(|| "add at least one pane before running".to_string())?;
        let Some(workspace) = self.workspaces.get(target_idx) else {
            return Err("open project workspace no longer exists".to_string());
        };
        if workspace.is_zoomed() {
            return Err("unzoom the project before running the template here".to_string());
        }
        if pane_specs.len() > MAX_PANES {
            return Err(format!("maximum pane count reached ({MAX_PANES})"));
        }

        let mut planned = pane_specs
            .iter()
            .enumerate()
            .map(|(idx, spec)| {
                parse_workspace_pane_plan(spec)
                    .map_err(|err| format!("pane {idx}: {}", err.message))
            })
            .collect::<Result<Vec<_>, _>>()?;
        dedupe_planned_pane_labels(&mut planned);

        let ws_id = self.workspaces[target_idx].id;
        let focus_idx = planned.iter().position(|plan| plan.focus).unwrap_or(0);
        let mut launches = Vec::with_capacity(planned.len());
        let mut panes = Vec::with_capacity(planned.len());
        for plan in planned {
            let env = stage_planned_pane_env(&plan, cx);
            let terminal = cx.new(|cx| {
                TerminalView::with_cwd_env_and_profile(
                    ws_id,
                    plan.cwd.clone(),
                    None,
                    env,
                    plan.profile,
                    cx,
                )
            });
            if let Some(label) = plan.label {
                terminal.update(cx, |view, _cx| {
                    view.terminal.custom_name = Some(label);
                });
            }
            let new_pane = self.create_pane(terminal.clone(), cx);
            launches.push((
                terminal,
                plan.command.filter(|command| !command.is_empty()),
                plan.prompt.filter(|prompt| !prompt.is_empty()),
            ));
            panes.push(new_pane);
        }
        let focus_pane = panes.get(focus_idx).cloned();
        let tree = build_up_layout(preset, panes, focus_idx)
            .ok_or_else(|| "could not build layout from panes".to_string())?;
        if let Some(workspace) = self.workspaces.get_mut(target_idx) {
            workspace.root = Some(tree);
            workspace.saved_layout = None;
            if let Some(first_cwd) = launches
                .iter()
                .find_map(|(terminal, _, _)| terminal.read(cx).terminal.cwd_now())
            {
                workspace.cwd = first_cwd.display().to_string();
            }
        }

        if self.active_idx != target_idx {
            self.active_idx = target_idx;
            self.reroot_files_tree(cx);
        }
        if let Some(pane) = focus_pane {
            self.pending_pane_focus = Some(pane);
        }
        for (pane_idx, (terminal, command, prompt)) in launches.into_iter().enumerate() {
            if let Some(command) = command {
                Self::schedule_launch_command(&terminal, command, prompt, pane_idx, cx);
            } else if let Some(prompt) = prompt {
                Self::schedule_prompt_prefill(&terminal, prompt, pane_idx, cx);
            }
        }
        self.save_session(cx);
        cx.notify();
        Ok(())
    }
}

pub(crate) fn workspace_cwd_matches(cwd: &str, project: &std::path::Path) -> bool {
    canonicalize_workspace_cwd(cwd)
        .ok()
        .is_some_and(|cwd| paths_equal(&cwd, project))
}

pub(crate) fn paths_equal(left: &std::path::Path, right: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
