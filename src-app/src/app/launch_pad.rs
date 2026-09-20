//! The Launch pad (`⇧⌘L`): the presets, and the editor for one.
//!
//! A 540-wide dialog with two faces in one frame. The **list** is one 64px
//! row per preset - its name, the parts it launches, the directories it
//! spans, its chord, `Edit`, `Start` - and a dashed
//! "+ save current layout as a preset" underneath. The **editor** replaces
//! the body: a name field, `PANES IN THIS PRESET` with one row per pane
//! (kind glyph, label, directory chip, `×`), "+ add a pane", and a footer of
//! `Delete preset` · `Cancel` · `Save preset`.
//!
//! # What used to be here
//!
//! A worktree launcher. `⇧⌘L` picked one agent out of sixteen and created a
//! git worktree on the way. Both of those have a home in the design, and both
//! are on the rail: `+ agent` is the agent menu, and `+ worktree` is the
//! dialog that landed before this file was rewritten. This chord was never
//! the design's Launch pad - it was two rail affordances glued behind a key -
//! and it is the presets' now.
//!
//! # Where the presets live
//!
//! In files, read by [`crate::preset::store`]. The dialog holds a snapshot
//! taken when it opened, so a slow folder never blocks a frame, and the
//! snapshot is refreshed after every save or delete. Editing works on a
//! **draft** [`Preset`]; nothing touches the file until `Save preset`.

use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, Styled, Window, deferred, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::preset::store::{self, StoredPreset};
use crate::preset::{PanePreset, Preset, PresetLayout};
use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;
use crate::widgets::text_input::TextInput;

/// The actions that start a preset by position, in that order.
///
/// Three fixed actions indexing the list, not a generated binding per preset:
/// the registry stays whole and rule P3 with it. A fourth preset is reachable
/// from the dialog like every other one.
pub(crate) const PRESET_CHORD_ACTIONS: [&str; 3] =
    ["start_preset_1", "start_preset_2", "start_preset_3"];

/// Live Launch pad state, owned by `SplitlaneApp`.
///
/// The presets themselves are not here: they live in `SplitlaneApp::presets`,
/// one cache with three readers, so the Settings list and the project row's
/// "Run preset" see the same folder this dialog does.
pub(crate) struct LaunchPadState {
    /// `Some` while the editor is up; the list otherwise.
    pub(crate) editing: Option<PresetDraft>,
    /// A sentence for the user - a launch that could not run, a save that
    /// failed, or what a file's reader set aside.
    pub(crate) status: Option<String>,
}

/// The preset being edited. Nothing here has reached disk yet.
pub(crate) struct PresetDraft {
    /// The file it came from. `None` for one that has never been saved.
    pub(crate) path: Option<PathBuf>,
    pub(crate) name_input: Entity<TextInput>,
    /// One per pane, index-aligned. A shell pane's row *is* its command -
    /// the mockup's label for a shell reads "pnpm dev" - so the label is the
    /// field, rather than a second control beside it. An agent pane's entry
    /// exists and is never drawn: its label is the agent's name, which is not
    /// something to type.
    pub(crate) command_inputs: Vec<Entity<TextInput>>,
    /// Which `[[panes]]` table of the file each pane came from, index-aligned;
    /// `None` for a pane added here. The writer needs this to move a pane's
    /// unshown keys - its `env`, its worktree fields - with the pane rather
    /// than with its position. See [`store::PaneOrigins`].
    pub(crate) origins: Vec<Option<usize>>,
    pub(crate) preset: Preset,
    /// Whether "+ add a pane" has its menu open.
    pub(crate) add_menu_open: bool,
}

/// What "+ add a pane" offers.
///
/// The design lists four: Claude session, Codex session, "Shell with a
/// command…", Diff review. Three are built. A diff is not a `workspace.up`
/// pane - every pane that method spawns is a terminal - so offering it here
/// would be an entry that cannot be honoured by the file it writes into.
/// So the fourth is left out rather than invented.
const ADD_PANE_CHOICES: &[(&str, &str)] = &[
    ("claude_code", "Claude session"),
    ("codex", "Codex session"),
    ("", "Shell with a command…"),
];

impl SplitlaneApp {
    pub(crate) fn handle_open_launch_pad(
        &mut self,
        _: &crate::OpenLaunchPad,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.launch_pad.is_some() {
            self.launch_pad = None;
            cx.notify();
            return;
        }
        self.launch_pad = Some(LaunchPadState {
            editing: None,
            status: None,
        });
        // The cache is filled at launch; re-read anyway, because a preset may
        // have been added in an editor since.
        self.reload_presets(cx);
        cx.notify();
    }

    /// The chord that starts the preset at `idx`, as this platform spells it
    /// and as the user has actually bound it.
    ///
    /// Read out of `effective_shortcuts` rather than formatted from a
    /// constant: `secondary-shift-1` is `⇧⌘1` on macOS and `Ctrl+Shift+1`
    /// everywhere else, and a user who rebound the action must see their own
    /// chord rather than the default. `None` when the action has no binding
    /// at all - the row then spends that space on the name.
    pub(crate) fn preset_chord_label(&self, idx: usize) -> Option<SharedString> {
        let action = PRESET_CHORD_ACTIONS.get(idx)?;
        self.effective_shortcuts
            .iter()
            .find(|entry| entry.action_name == *action)
            .map(|entry| entry.key.as_str())
            .filter(|key| *key != "Unassigned")
            .map(SharedString::from)
    }

    /// Re-read the folder off the render thread into the app's cache.
    ///
    /// Every reader of a preset goes through that cache, so nothing touches
    /// the disk during a frame. Called at launch, when the Launch pad opens,
    /// when the Presets settings section opens, and after every save or
    /// delete.
    pub(crate) fn reload_presets(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let presets = smol::unblock(store::load_all).await;
            let _ = this.update(cx, |app, cx| {
                // A file's warnings are said once, in the dialog where its
                // preset is listed - the alternative is a reader that
                // silently drops a key it did not recognise.
                let warning = presets.iter().find_map(|stored| {
                    stored
                        .warnings
                        .first()
                        .map(|w| format!("{}: {w}", stored.id))
                });
                app.presets = presets;
                if let Some(lp) = app.launch_pad.as_mut()
                    && lp.status.is_none()
                    && let Some(warning) = warning
                {
                    lp.status = Some(warning);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn launch_pad_close(&mut self, cx: &mut Context<Self>) {
        self.launch_pad = None;
        cx.notify();
    }

    /// `⇧⌘1-3`: start the preset at that position, whether the dialog is open
    /// or not. The chord follows position in the list, so reordering the
    /// folder reassigns them - which is what the design says it does.
    pub(crate) fn start_preset_by_index(&mut self, idx: usize, cx: &mut Context<Self>) {
        // The chord can fire with no dialog open, so the folder may not have
        // been read yet. Read it, then act.
        if !self.presets.is_empty() {
            let stored = self.presets.get(idx).cloned();
            self.start_stored_preset(stored, cx);
            return;
        }
        // Nothing cached yet - the chord can fire before the launch read
        // lands. Read the folder, then act.
        cx.spawn(async move |this, cx| {
            let presets = smol::unblock(store::load_all).await;
            let _ = this.update(cx, |app, cx| {
                app.presets = presets;
                let stored = app.presets.get(idx).cloned();
                app.start_stored_preset(stored, cx);
            });
        })
        .detach();
    }

    pub(crate) fn handle_start_preset_1(
        &mut self,
        _: &crate::StartPreset1,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_preset_by_index(0, cx);
    }

    pub(crate) fn handle_start_preset_2(
        &mut self,
        _: &crate::StartPreset2,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_preset_by_index(1, cx);
    }

    pub(crate) fn handle_start_preset_3(
        &mut self,
        _: &crate::StartPreset3,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_preset_by_index(2, cx);
    }

    fn start_stored_preset(&mut self, stored: Option<StoredPreset>, cx: &mut Context<Self>) {
        let Some(stored) = stored else {
            self.show_toast("No preset in that slot", cx);
            return;
        };
        match self.run_preset(&stored.preset, cx) {
            Ok(()) => {
                let name = stored.preset.display_name().to_string();
                self.launch_pad = None;
                self.show_toast(format!("{name} started"), cx);
            }
            Err(message) => {
                if let Some(lp) = self.launch_pad.as_mut() {
                    lp.status = Some(message);
                } else {
                    self.show_toast(message, cx);
                }
            }
        }
        cx.notify();
    }

    fn launch_pad_edit(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stored) = self.presets.get(idx).cloned() else {
            return;
        };
        self.open_preset_draft(Some(stored.path), stored.preset, window, cx);
    }

    fn open_preset_draft(
        &mut self,
        path: Option<PathBuf>,
        preset: Preset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = preset.display_name().to_string();
        let name_input = cx.new(|cx| TextInput::new(initial, "Preset name", cx));
        let focus = name_input.read(cx).focus_handle.clone();
        let command_inputs = preset
            .panes
            .iter()
            .map(|pane| command_input(pane, cx))
            .collect();
        // A preset read from a file starts out in file order, one pane per
        // table; one built from the open panes has no file behind it at all.
        let origins = match path {
            Some(_) => (0..preset.panes.len()).map(Some).collect(),
            None => vec![None; preset.panes.len()],
        };
        if let Some(lp) = self.launch_pad.as_mut() {
            lp.status = None;
            lp.editing = Some(PresetDraft {
                path,
                name_input,
                command_inputs,
                origins,
                preset,
                add_menu_open: false,
            });
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    /// "+ save current layout as a preset": the active project's panes, in
    /// the arrangement they are in, as a draft the user then names.
    ///
    /// A preset is a recipe, so what is copied is what a recipe can hold: a
    /// pane's directory and the agent it runs. A shell's live command is not
    /// read off the grid - the pane's own "↻ rerun" is the only place that
    /// knows one, and it is frequently absent - so a shell pane starts blank
    /// and the user types the command they meant.
    fn save_current_layout_as_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(container) = self.workspaces.get(self.active_idx) else {
            return;
        };
        let name = container.title.clone();
        let cwd = container.cwd.clone();
        let Some(root) = container.root.as_ref() else {
            if let Some(lp) = self.launch_pad.as_mut() {
                lp.status = Some("This project has no panes yet".to_string());
            }
            cx.notify();
            return;
        };
        // `Horizontal` names the divider bar, not the arrangement: it is the
        // stacked one. The grid is asked about first, because it IS a
        // horizontal split at the root and would otherwise be saved as a column
        // - a preset that quietly came back in a different form from the one it
        // was saved from.
        let layout = if root.is_grid() {
            PresetLayout::Grid
        } else {
            match root.root_direction() {
                Some(crate::layout::SplitDirection::Horizontal) => PresetLayout::EvenV,
                _ => PresetLayout::EvenH,
            }
        };
        let leaves = root.collect_leaves();
        let agents_by_thread: std::collections::HashMap<u64, &'static str> = container
            .threads
            .iter()
            .filter_map(|thread| Some((thread.id, thread.terminal_agent?.tag())))
            .collect();
        let panes: Vec<PanePreset> = leaves
            .iter()
            .map(|pane| {
                let agent = pane
                    .read(cx)
                    .active_terminal_opt()
                    .and_then(|terminal| terminal.read(cx).agent_thread_id)
                    .and_then(|id| agents_by_thread.get(&id).copied())
                    .map(str::to_string);
                PanePreset {
                    agent,
                    ..Default::default()
                }
            })
            .collect();
        let preset = Preset {
            name: Some(name),
            layout,
            cwd: Some(cwd),
            color: None,
            port_base: None,
            panes,
        };
        self.open_preset_draft(None, preset, window, cx);
    }

    fn draft_add_pane(&mut self, agent_tag: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.launch_pad.as_mut().and_then(|lp| lp.editing.as_mut()) else {
            return;
        };
        draft.add_menu_open = false;
        if draft.preset.panes.len() >= crate::layout::MAX_PANES {
            if let Some(lp) = self.launch_pad.as_mut() {
                lp.status = Some(format!(
                    "A project holds at most {} panes",
                    crate::layout::MAX_PANES
                ));
            }
            cx.notify();
            return;
        }
        let pane = PanePreset {
            agent: (!agent_tag.is_empty()).then(|| agent_tag.to_string()),
            ..Default::default()
        };
        let input = command_input(&pane, cx);
        let focus = input.read(cx).focus_handle.clone();
        let is_shell = pane.agent.is_none();
        if let Some(draft) = self.launch_pad.as_mut().and_then(|lp| lp.editing.as_mut()) {
            draft.preset.panes.push(pane);
            draft.command_inputs.push(input);
            draft.origins.push(None);
        }
        // "Shell with a command…" ends in an ellipsis because it asks for
        // one: the new row opens with the caret already in it.
        if is_shell {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn draft_remove_pane(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let Some(draft) = self.launch_pad.as_mut().and_then(|lp| lp.editing.as_mut())
            && idx < draft.preset.panes.len()
        {
            draft.preset.panes.remove(idx);
            draft.command_inputs.remove(idx);
            draft.origins.remove(idx);
            cx.notify();
        }
    }

    fn draft_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(lp) = self.launch_pad.as_mut() {
            lp.editing = None;
            lp.status = None;
        }
        cx.notify();
    }

    fn draft_save(&mut self, cx: &mut Context<Self>) {
        let Some(lp) = self.launch_pad.as_mut() else {
            return;
        };
        let Some(draft) = lp.editing.as_mut() else {
            return;
        };
        draft.preset.name = Some(draft.name_input.read(cx).value().trim().to_string());
        for (idx, pane) in draft.preset.panes.iter_mut().enumerate() {
            if pane.agent.is_some() {
                continue;
            }
            pane.command = draft
                .command_inputs
                .get(idx)
                .map(|input| input.read(cx).value().trim().to_string())
                .filter(|command| !command.is_empty());
        }
        let preset = draft.preset.clone();
        let origins = draft.origins.clone();
        if let Err(message) = preset.validate() {
            lp.status = Some(message);
            cx.notify();
            return;
        }
        let path = match draft.path.clone() {
            Some(path) => path,
            None => {
                let Some(dir) = store::presets_dir() else {
                    lp.status = Some("No config directory on this platform".to_string());
                    cx.notify();
                    return;
                };
                if let Err(err) = std::fs::create_dir_all(&dir) {
                    lp.status = Some(format!("{}: {err}", dir.display()));
                    cx.notify();
                    return;
                }
                store::unused_path(&dir, preset.display_name())
            }
        };
        lp.editing = None;
        lp.status = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || store::save(&path, &preset, &origins)).await;
            let _ = this.update(cx, |app, cx| {
                if let Err(message) = result
                    && let Some(lp) = app.launch_pad.as_mut()
                {
                    lp.status = Some(message);
                }
                app.reload_presets(cx);
            });
        })
        .detach();
    }

    fn draft_delete(&mut self, cx: &mut Context<Self>) {
        let Some(lp) = self.launch_pad.as_mut() else {
            return;
        };
        let path = lp.editing.as_ref().and_then(|draft| draft.path.clone());
        lp.editing = None;
        lp.status = None;
        cx.notify();
        let Some(path) = path else {
            // Never written; there is nothing to delete.
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
            })
            .await;
            let _ = this.update(cx, |app, cx| {
                if let Err(message) = result
                    && let Some(lp) = app.launch_pad.as_mut()
                {
                    lp.status = Some(message);
                }
                app.reload_presets(cx);
            });
        })
        .detach();
    }

    pub(crate) fn handle_launch_pad_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                let editing = self
                    .launch_pad
                    .as_ref()
                    .is_some_and(|lp| lp.editing.is_some());
                if editing {
                    self.draft_cancel(cx);
                } else {
                    self.launch_pad_close(cx);
                }
            }
            "enter"
                if self
                    .launch_pad
                    .as_ref()
                    .is_some_and(|lp| lp.editing.is_some()) =>
            {
                self.draft_save(cx)
            }
            _ => {}
        }
    }

    pub(crate) fn render_launch_pad(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(lp) = self.launch_pad.as_ref() else {
            return div().into_any_element();
        };
        let ui = crate::theme::ui_colors();

        let project: SharedString = self
            .workspaces
            .get(self.active_idx)
            .map(|w| SharedString::from(w.title.clone()))
            .unwrap_or_default();

        let mut card = div()
            .id("launch-pad")
            .occlude()
            .track_focus(&self.launch_pad_focus)
            .on_key_down(cx.listener(Self::handle_launch_pad_key_down))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.launch_pad_close(cx)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .w(px(540.))
            .flex()
            .flex_col()
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .rounded(tok::radius::WINDOW)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(tok::space::MD)
                    .px(tok::space::SECTION)
                    .pt(tok::space::XXL)
                    .pb(tok::space::LG)
                    .border_b_1()
                    .border_color(ui.border)
                    .child(
                        div()
                            .text_size(tok::text::TITLE)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(ui.text)
                            .child("Launch pad"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::LABEL)
                            .text_color(ui.dim)
                            .truncate()
                            .child(project),
                    ),
            );

        card = match lp.editing.as_ref() {
            Some(draft) => card
                .child(self.render_preset_editor(draft, ui, cx))
                .child(self.render_editor_footer(ui, cx)),
            None => card.child(self.render_preset_list(ui, cx)),
        };

        if let Some(status) = lp.status.as_ref() {
            card = card.child(
                div()
                    .px(tok::space::SECTION)
                    .pb(tok::space::XL)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.vc_deleted)
                    .child(status.clone()),
            );
        }

        deferred(
            div()
                .id("launch-pad-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(140.))
                .bg(ui.scrim)
                .child(card),
        )
        .with_priority(8)
        .into_any_element()
    }

    fn render_preset_list(&self, ui: crate::theme::UiColors, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .flex()
            .flex_col()
            .gap(tok::space::MD)
            .p(tok::space::XL);

        if self.presets.is_empty() {
            list = list.child(
                div()
                    .py(tok::space::XL)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted)
                    .child(
                        "No presets yet. Save the panes you have open, or drop a \
                         .toml in the presets folder - the same file runs from \
                         `splitlane up`.",
                    ),
            );
        }

        for (idx, stored) in self.presets.iter().enumerate() {
            list = list.child(self.render_preset_row(idx, stored, ui, cx));
        }

        list.child(
            div()
                .id("launch-pad-save-current")
                .flex()
                .items_center()
                .h(px(34.))
                .px(tok::space::XL)
                .rounded(tok::radius::PANEL)
                .border_1()
                .border_dashed()
                .border_color(ui.border_strong)
                .font_family(tok::font::MONO)
                .text_size(tok::mono::LABEL)
                .text_color(ui.dim)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.save_current_layout_as_preset(window, cx);
                    cx.stop_propagation();
                }))
                .child("+ save current layout as a preset"),
        )
        .into_any_element()
    }

    fn render_preset_row(
        &self,
        idx: usize,
        stored: &StoredPreset,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preset = &stored.preset;
        let parts: SharedString = SharedString::from(
            preset
                .panes
                .iter()
                .map(pane_label)
                .collect::<Vec<_>>()
                .join(" · "),
        );
        // Home-collapsed, like every other path this app shows: the row has
        // one line for however many directories the preset spans.
        let dirs: SharedString = SharedString::from(
            preset
                .directories()
                .iter()
                .map(|dir| crate::app::sidebar::collapse_home(dir, &self.home_dir))
                .collect::<Vec<_>>()
                .join(" · "),
        );
        let chord: SharedString = self.preset_chord_label(idx).unwrap_or_default();

        div()
            .id(SharedString::from(format!("launch-pad-row-{}", stored.id)))
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XL)
            .h(px(64.))
            .px(tok::space::XL)
            .rounded(tok::radius::PANEL)
            .border_1()
            .border_color(ui.border)
            .bg(ui.surface)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .truncate()
                            .text_size(tok::text::ROW)
                            .text_color(ui.text)
                            .child(SharedString::from(preset.display_name().to_string())),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::LABEL)
                            .text_color(ui.dim)
                            .child(parts),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::LABEL)
                            .text_color(ui.faint)
                            .child(dirs),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(ui.muted)
                    .child(chord),
            )
            .child(quiet_button(
                SharedString::from(format!("launch-pad-edit-{}", stored.id)),
                "Edit",
                ui,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.launch_pad_edit(idx, window, cx);
                    cx.stop_propagation();
                }),
            ))
            .child(accent_button(
                SharedString::from(format!("launch-pad-start-{}", stored.id)),
                "Start",
                ui,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    let stored = this.presets.get(idx).cloned();
                    this.start_stored_preset(stored, cx);
                    cx.stop_propagation();
                }),
            ))
            .into_any_element()
    }

    fn render_preset_editor(
        &self,
        draft: &PresetDraft,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = div()
            .flex()
            .flex_col()
            .px(tok::space::SECTION)
            .py(tok::space::XXL)
            .child(
                div()
                    .pb(tok::space::SM)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.muted)
                    .child("Preset name"),
            )
            .child(
                div()
                    .mb(tok::space::SECTION)
                    .border_1()
                    .border_color(ui.border)
                    .rounded(tok::radius::CONTROL)
                    .px(tok::space::LG)
                    .py(tok::space::MD)
                    .child(draft.name_input.clone()),
            )
            .child(
                div()
                    .pb(tok::space::MD)
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(ui.muted)
                    .child("PANES IN THIS PRESET"),
            );

        for (idx, pane) in draft.preset.panes.iter().enumerate() {
            body = body.child(self.render_draft_pane_row(
                idx,
                pane,
                draft.command_inputs.get(idx),
                ui,
                cx,
            ));
        }

        let mut caption = String::from(
            "Each pane carries its own directory, so a preset can span two repositories. \
             Blank means the project it was saved from. The same file runs from \
             `splitlane up`.",
        );
        if draft
            .preset
            .panes
            .iter()
            .any(|pane| !unshown_fields(pane).is_empty())
        {
            // The one sentence that makes the dim line under a pane readable,
            // and the refusal it foretells honest.
            caption.push_str(
                " A worktree and a `${port_offset}` port are created by `splitlane up` \
                 and by nothing else, so panes asking for them are shown here as the \
                 file has them and start from that command rather than from here.",
            );
        }
        body = body.child(self.render_add_pane(draft, ui, cx)).child(
            div()
                .pt(tok::space::XXL)
                .text_size(tok::text::CAPTION)
                .text_color(ui.muted)
                .child(caption),
        );
        body.into_any_element()
    }

    fn render_draft_pane_row(
        &self,
        idx: usize,
        pane: &PanePreset,
        command_input: Option<&Entity<TextInput>>,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cwd: SharedString = SharedString::from(
            pane.cwd
                .as_deref()
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
                .unwrap_or("project")
                .to_string(),
        );
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::LG)
            .h(px(34.))
            .px(tok::space::LG)
            .rounded(tok::radius::CONTROL)
            .border_1()
            .border_color(ui.border)
            .bg(ui.surface)
            .child(
                div()
                    .flex_none()
                    .text_size(tok::text::CAPTION)
                    .text_color(if pane.agent.is_some() {
                        ui.accent
                    } else {
                        ui.text_secondary
                    })
                    .child(pane_glyph(pane)),
            )
            .child(match (pane.agent.is_some(), command_input) {
                // An agent's label is its name; there is nothing to type.
                (true, _) | (false, None) => div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::ROW)
                    .text_color(ui.text_secondary)
                    .child(SharedString::from(pane_label(pane))),
                // A shell's label *is* its command, so the label is the field.
                (false, Some(input)) => div()
                    .flex_1()
                    .min_w_0()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::ROW)
                    .text_color(ui.text_secondary)
                    .child(input.clone()),
            })
            .child(
                div()
                    .flex_none()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::HINT)
                    .text_color(ui.faint)
                    .child(cwd),
            )
            .child(
                div()
                    .id(SharedString::from(format!("launch-pad-pane-remove-{idx}")))
                    .flex_none()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(tok::radius::BADGE)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.dim)
                    .cursor_pointer()
                    .animated_hover(move |style, delta| {
                        style.text_color(crate::ui_primitives::lerp_color(ui.dim, ui.text, delta));
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.draft_remove_pane(idx, cx);
                        cx.stop_propagation();
                    }))
                    .child("×"),
            );

        let mut stack = div().flex().flex_col().mb(tok::space::SM).child(row);
        // What the file asks for and this editor has no control for. Shown
        // because the builder now *refuses* a pane over these fields, and a
        // person cannot be refused over a field they have never been shown.
        let unshown = unshown_fields(pane);
        if !unshown.is_empty() {
            stack = stack.child(
                div()
                    .min_w_0()
                    .pt(tok::space::XS)
                    .px(tok::space::LG)
                    .truncate()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::PATH)
                    .text_color(ui.faint)
                    .child(SharedString::from(unshown.join("  ·  "))),
            );
        }
        stack.into_any_element()
    }

    fn render_add_pane(
        &self,
        draft: &PresetDraft,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div()
            .id("launch-pad-add-pane")
            .relative()
            .flex()
            .items_center()
            .h(px(32.))
            .px(tok::space::LG)
            .rounded(tok::radius::CONTROL)
            .border_1()
            .border_dashed()
            .border_color(ui.border_strong)
            .font_family(tok::font::MONO)
            .text_size(tok::mono::LABEL)
            .text_color(ui.dim)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                if let Some(draft) = this.launch_pad.as_mut().and_then(|lp| lp.editing.as_mut()) {
                    draft.add_menu_open = !draft.add_menu_open;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .child("+ add a pane");

        if draft.add_menu_open {
            let mut menu = crate::settings::components::select_menu("launch-pad-add-menu", ui)
                .occlude()
                .absolute()
                .left_0()
                .top(px(34.))
                .w(px(240.))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if let Some(draft) = this.launch_pad.as_mut().and_then(|lp| lp.editing.as_mut())
                    {
                        draft.add_menu_open = false;
                        cx.notify();
                    }
                }));
            for (tag, label) in ADD_PANE_CHOICES {
                menu = menu.child(self.render_select_menu_item(
                    SharedString::from(format!("launch-pad-add-{label}")),
                    label,
                    None,
                    ui,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.draft_add_pane(tag, window, cx);
                        cx.stop_propagation();
                    }),
                ));
            }
            row = row.child(deferred(menu).with_priority(9));
        }
        row.into_any_element()
    }

    fn render_editor_footer(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(tok::space::SECTION)
            .py(tok::space::XL)
            .border_t_1()
            .border_color(ui.border)
            .bg(ui.chrome)
            .child(danger_button(
                "launch-pad-delete".into(),
                "Delete preset",
                ui,
                cx.listener(|this, _: &ClickEvent, _w, cx| {
                    this.draft_delete(cx);
                    cx.stop_propagation();
                }),
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(tok::space::MD)
                    .child(quiet_button(
                        "launch-pad-cancel".into(),
                        "Cancel",
                        ui,
                        cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.draft_cancel(cx);
                            cx.stop_propagation();
                        }),
                    ))
                    .child(accent_button(
                        "launch-pad-save".into(),
                        "Save preset",
                        ui,
                        cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.draft_save(cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
            .into_any_element()
    }
}

/// The field behind a shell pane's label.
///
/// Built for every pane so the two lists stay index-aligned; an agent's is
/// never drawn.
fn command_input(pane: &PanePreset, cx: &mut Context<SplitlaneApp>) -> Entity<TextInput> {
    let initial = pane.command.clone().unwrap_or_default();
    cx.new(|cx| TextInput::new(initial, "shell command", cx))
}

/// The kind glyph: ◆ an agent, ▷ a shell.
///
/// The design's third, ± for a diff, has nothing to mark yet - see
/// [`ADD_PANE_CHOICES`].
fn pane_glyph(pane: &PanePreset) -> &'static str {
    if pane.agent.is_some() { "◆" } else { "▷" }
}

/// What a pane launches, in a word: the agent's name, the command, or the
/// fact that it is a shell with nothing typed into it yet.
fn pane_label(pane: &PanePreset) -> String {
    if let Some(tag) = pane.agent.as_deref().filter(|tag| !tag.trim().is_empty()) {
        return crate::preset::resolve_agent(tag)
            .map(|agent| agent.display_name().to_string())
            .unwrap_or_else(|| tag.to_string());
    }
    pane.command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .unwrap_or("shell")
        .to_string()
}

/// The pane's fields this editor draws no control for, in file order.
///
/// The editor has always **saved** them without showing them
/// (`store::PaneOrigins` carries a pane's unshown keys through a save), which
/// was harmless while nothing looked at them and stopped being harmless the
/// moment the builder started refusing a pane over `worktree` and over an
/// unallocated `${port_offset}`: a refusal that names a field the person has
/// never seen reads as a caprice.
///
/// Read-only on purpose. Making them editable is a larger decision - the
/// Launch pad's editor has a designed form, and `setup` is a shell command
/// with a timeout beside it - and it is not what this closes.
fn unshown_fields(pane: &PanePreset) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(branch) = trimmed_field(pane.worktree.as_deref()) {
        out.push(format!("worktree {branch}"));
    }
    if let Some(setup) = trimmed_field(pane.setup.as_deref()) {
        out.push(format!("setup {setup}"));
    }
    if let Some(teardown) = trimmed_field(pane.worktree_teardown.as_deref()) {
        out.push(format!("teardown {teardown}"));
    }
    // The second field the builder refuses over, named the same way for the
    // same reason: one entry, listing the variables that reference it rather
    // than dumping the pane's whole env.
    if let Some(env) = pane.env.as_ref() {
        let mut keys: Vec<&str> = env
            .iter()
            .filter(|(_, value)| value.contains(crate::preset::PORT_OFFSET_TOKEN))
            .map(|(key, _)| key.as_str())
            .collect();
        keys.sort_unstable();
        if !keys.is_empty() {
            // Every variable that wants one, because the refusal names every
            // one of them and the two may not disagree.
            out.push(format!("port {}", keys.join(", ")));
        }
    }
    out
}

fn trimmed_field(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn button_frame(id: SharedString) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .h(px(26.))
        .px(tok::space::LG)
        .rounded(tok::radius::SMALL)
        .text_size(tok::text::CONTROL)
        .cursor_pointer()
}

fn quiet_button(
    id: SharedString,
    label: &'static str,
    ui: crate::theme::UiColors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let resting = ui.border;
    let hovered = ui.border_hover;
    button_frame(id)
        .border_1()
        .border_color(resting)
        .text_color(ui.text_secondary)
        .animated_hover(move |style, delta| {
            style.border_color(crate::ui_primitives::lerp_color(resting, hovered, delta));
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child(label)
        .into_any_element()
}

fn accent_button(
    id: SharedString,
    label: &'static str,
    ui: crate::theme::UiColors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    button_frame(id)
        .bg(ui.accent)
        // The fill's own luminance answers this, so a light theme's accent
        // does not get white text on it.
        .text_color(crate::theme::text_on_fill(ui.accent))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .animated_hover(|style, delta| {
            style.opacity(1.0 - 0.15 * delta);
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child(label)
        .into_any_element()
}

fn danger_button(
    id: SharedString,
    label: &'static str,
    ui: crate::theme::UiColors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let resting = ui.border;
    let danger = ui.vc_deleted;
    button_frame(id)
        .border_1()
        .border_color(resting)
        .text_color(ui.text_secondary)
        .animated_hover(move |style, delta| {
            style
                .border_color(crate::ui_primitives::lerp_color(resting, danger, delta))
                .text_color(crate::ui_primitives::lerp_color(
                    ui.text_secondary,
                    danger,
                    delta,
                ));
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_says_what_it_launches() {
        let agent = PanePreset {
            agent: Some("claude_code".to_string()),
            ..Default::default()
        };
        assert_eq!(pane_label(&agent), "Claude Code");
        assert_eq!(pane_glyph(&agent), "◆");

        let shell = PanePreset {
            command: Some("pnpm dev".to_string()),
            ..Default::default()
        };
        assert_eq!(pane_label(&shell), "pnpm dev");
        assert_eq!(pane_glyph(&shell), "▷");

        assert_eq!(pane_label(&PanePreset::default()), "shell");
    }

    #[test]
    fn a_pane_shows_the_fields_the_editor_has_no_control_for() {
        let mut env = std::collections::HashMap::new();
        env.insert("API_PORT".to_string(), "${port_offset}".to_string());
        env.insert("QUIET".to_string(), "1".to_string());
        let pane = PanePreset {
            cwd: Some("/repo".to_string()),
            agent: Some("claude_code".to_string()),
            worktree: Some("feat/x".to_string()),
            setup: Some("bun install".to_string()),
            worktree_teardown: Some("keep".to_string()),
            env: Some(env),
            ..Default::default()
        };
        assert_eq!(
            unshown_fields(&pane),
            vec![
                "worktree feat/x",
                "setup bun install",
                "teardown keep",
                // One entry, naming the variables that want the port.
                "port API_PORT",
            ]
        );
    }

    #[test]
    fn an_ordinary_pane_carries_no_such_line() {
        let pane = PanePreset {
            command: Some("pnpm dev".to_string()),
            ..Default::default()
        };
        assert!(unshown_fields(&pane).is_empty());

        // A blank field is not a field: the writer removes a key rather than
        // writing an empty string, but a hand-edited file may still hold one.
        let blank = PanePreset {
            command: Some("pnpm dev".to_string()),
            worktree: Some("  ".to_string()),
            ..Default::default()
        };
        assert!(unshown_fields(&blank).is_empty());
    }

    #[test]
    fn an_unknown_agent_tag_shows_itself_rather_than_vanishing() {
        let pane = PanePreset {
            agent: Some("some-future-cli".to_string()),
            ..Default::default()
        };
        assert_eq!(pane_label(&pane), "some-future-cli");
    }
}
