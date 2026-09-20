//! "Presets" settings page: a read-only list, and the folder it reads.
//!
//! It used to be a 2 200-line editor - a preset picker, a layout dropdown, a
//! pane inspector with four text fields, and its own builder for the
//! `workspace.up` request. All of that is in the Launch pad now, which is
//! where the design puts it, and which is one gesture away from the panes the
//! preset is about rather than four clicks into a settings tree.
//!
//! What stays is what a settings section is for: a statement of what exists.
//! One row per preset - "3 panes · ~/work/atlas", with the chord if it has
//! one - a line saying where editing happens, and "reveal presets folder",
//! because the files are the thing and the folder is the only affordance that
//! cannot be described in a row.

use gpui::{ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div};

use crate::SplitlaneApp;
use crate::preset::store;
use crate::settings::components::{ChipTone, setting_action_row, setting_note, setting_row};

impl SplitlaneApp {
    pub(crate) fn render_presets_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let mut page = div().flex().flex_col().child(setting_note(
            ui,
            SharedString::from(format!(
                "A preset is a saved set of panes, each with its own directory \u{2014} so one \
                 preset can span two repositories. Editing happens in the Launch pad ({}); \
                 the same file drives `splitlane up`.",
                crate::keybindings::format_keystroke("secondary-shift-l"),
            )),
        ));

        if self.presets.is_empty() {
            page = page.child(setting_row(
                ui,
                "Presets",
                Some(SharedString::from(
                    "save the panes you have open from the Launch pad",
                )),
                "none yet",
                ChipTone::Off,
            ));
        }

        for (idx, stored) in self.presets.iter().enumerate() {
            let preset = &stored.preset;
            let dirs = preset.directories();
            // Two directories are named by their count, not listed: the row
            // has one line, and "2 directories" is the fact that matters.
            let where_ = match dirs.len() {
                0 => "no directory".to_string(),
                1 => crate::app::sidebar::collapse_home(&dirs[0], &self.home_dir),
                n => format!("{n} directories"),
            };
            let panes = match preset.panes.len() {
                1 => "1 pane".to_string(),
                n => format!("{n} panes"),
            };
            let chord = self.preset_chord_label(idx);
            page = page.child(setting_row(
                ui,
                SharedString::from(preset.display_name().to_string()),
                chord,
                SharedString::from(format!("{panes} \u{00b7} {where_}")),
                ChipTone::Plain,
            ));
        }

        page.child(setting_action_row(
            ui,
            "presets-reveal-folder",
            "reveal presets folder",
            store::presets_dir().is_some(),
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.reveal_presets_folder(cx);
            }),
        ))
    }

    /// Open the presets folder in the file manager, creating it first.
    ///
    /// Creating it is the point: on a machine that has never saved a preset
    /// the folder does not exist, and revealing nothing would read as a
    /// broken button rather than as an empty library.
    fn reveal_presets_folder(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = store::presets_dir() else {
            self.show_toast("No config directory on this platform", cx);
            return;
        };
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.show_toast(format!("{}: {err}", dir.display()), cx);
            return;
        }
        if let Err(message) = crate::app::workspace_ops::reveal_in_file_manager(&dir) {
            self.show_toast(message, cx);
        }
    }
}
