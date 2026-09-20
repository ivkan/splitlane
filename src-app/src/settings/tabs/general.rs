//! "General" settings page - the default landing section.
//!
//! Nine rows in the design's row anatomy (`components::setting_*_row`), and
//! the audit behind them is worth writing down, because two thirds of this
//! section states facts rather than offering choices:
//!
//! - **Default editor** (`external_editor`) - a choice. The app used to open
//!   files and folders. The design's General has no editor row at all, because
//!   the design has no "open it in your editor" action; ours does (the
//!   Files panel and the rail menus), so the preference stays and is marked as
//!   a divergence rather than deleted.
//! - **Check for updates** (`check_for_updates`) - a choice, and one that had
//!   no screen at all until now: the key was read at startup by `bootstrap` and
//!   settable only by editing `splitlane.json` by hand.
//! - **Restore the last layout on launch**, **Reattach agent sessions**,
//!   **Confirm before closing a running session**, **Save layout changes** -
//!   facts. All four are true and none is a preference: restore is
//!   unconditional, a restored agent surface resumes from the CLI's own
//!   transcript, deleting a session always asks, and the layout is written to
//!   `session.json` as it changes. The design takes the stated chip over a
//!   switch and puts these four in this order, with the leading note calling
//!   them behaviour - a switch over something that cannot be switched is a lie.
//! - **Data directory** and **Permission mode** - facts about the machine, not
//!   about this app's config. The mode belongs to the agent's session and is
//!   the CLI's to decide; Splitlane states it and does not set it.
//!
//! Three rows are gone with the permission bar: "Answer permission asks in
//! Splitlane" (`harbor_answers_permission_asks`), the scope it stated (who
//! the app answered for) and the count of the user's own `allow` rules that ran before
//! it. An agent asks in its own terminal, which is what the closing caption now
//! says without an "off" to qualify.
//!
//! The shell moved out to Terminal, where the design puts it.
//!
//! Choices persist through [`SplitlaneApp::persist_setting`] (cache-mutate,
//! repaint, off-thread write); the editor menu closes on select, on
//! click-outside, on the trigger, on Escape, and on a tab change.

use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, IntoElement, ParentElement, SharedString, Styled,
    div, prelude::*, px,
};
use serde_json::Value;

use crate::GeneralDropdown;
use crate::SplitlaneApp;
use crate::settings::components::{
    ChipTone, Logo, SETTING_ROW_MAX_WIDTH, render_logo, select_item, select_menu,
    setting_choice_row, setting_note, setting_row, setting_toggle_row, value_chip,
};
use crate::ui_tokens as tok;

/// One select option: display label, optional leading logo, the JSON value
/// written to config when picked, and whether it is the current selection.
pub(crate) type SelectOption = (String, Option<Logo>, Value, bool);

impl SplitlaneApp {
    pub(crate) fn render_general_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let config = &self.cached_config;

        // ── Default editor (external_editor) ────────────────────────────
        // "auto" is the default when unset. Each preset carries its brand logo
        // (see `editor_icon`).
        let editor_value = config
            .external_editor
            .clone()
            .unwrap_or_else(|| "auto".to_string());
        let editor_opts: Vec<SelectOption> = EDITOR_PRESETS
            .iter()
            .map(|(label, val)| {
                (
                    (*label).to_string(),
                    editor_icon(val),
                    Value::String((*val).to_string()),
                    editor_value == *val,
                )
            })
            .collect();
        let editor_label = editor_opts
            .iter()
            .find(|(_, _, _, selected)| *selected)
            .map(|(label, _, _, _)| label.clone())
            .unwrap_or_else(|| editor_value.clone());

        let editor_row = self.general_choice_row(
            GeneralDropdown::Editor,
            "Default editor",
            None,
            editor_label,
            editor_opts,
            "external_editor",
            ui,
            cx,
        );

        // ── Check for updates ───────────────────────────────────────────
        // `None` means "check": the release feed is only skipped on an explicit
        // `false` (see `bootstrap`), so the row has to resolve the same way.
        let updates_on = config.check_for_updates != Some(false);
        let updates_row = setting_toggle_row(
            ui,
            "general-updates",
            "Check for updates",
            Some(SharedString::from("stable channel")),
            updates_on,
            cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.persist_setting(false, "check_for_updates", Value::Bool(!updates_on), cx);
            }),
        );

        let data_dir = crate::runtime_paths::data_dir()
            .map(|dir| tilde(&dir.display().to_string()))
            .unwrap_or_else(|| "unresolved".to_string());

        // Order is the design's: the four behaviour rows first, so the leading
        // note's "the first four rows state behaviour" is true of what is drawn.
        // Our two extra choices (the editor, the update check) follow them.
        //
        // The stated rows take the plain tone, not the accent one. "Always" is
        // true of all four, but an accent chip is the design's word for "on,
        // and you turned it on" - accent chips on rows that do nothing when
        // pressed would spend the section's one strong colour on the rows that
        // are not choices.
        let rows = div()
            .flex()
            .flex_col()
            .child(setting_row(
                ui,
                "Restore the last layout on launch",
                None,
                "always",
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "Reattach agent sessions",
                Some(SharedString::from("transcripts from disk")),
                "always",
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "Confirm before closing a running session",
                Some(SharedString::from("closing its tab leaves it running")),
                "always",
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "Save layout changes",
                Some(SharedString::from("session.json")),
                "always",
                ChipTone::Plain,
            ))
            .child(updates_row)
            .child(editor_row)
            .child(setting_row(
                ui,
                "Data directory",
                None,
                data_dir,
                ChipTone::Plain,
            ))
            // A fact about the agent's session, not a setting of ours: the
            // CLI decides when it asks, and it asks in its own terminal. The
            // subtitle used to end "\u{2014} for edits it does not", which was
            // true only while the app was filling that silence.
            .child(setting_row(
                ui,
                "Permission mode",
                Some(SharedString::from("the CLI decides when to ask")),
                "per session",
                ChipTone::Plain,
            ));

        div()
            .flex()
            .flex_col()
            .child(setting_note(
                ui,
                "Splitlane keeps one window per machine. Projects are folders on disk \u{2014} \
                 nothing is copied or imported. The first four rows state behaviour: they are \
                 always on and there is nothing to switch.",
            ))
            .child(rows)
            .child(
                div()
                    .pt(tok::space::XL)
                    .max_w(px(SETTING_ROW_MAX_WIDTH))
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.text_tertiary)
                    .child(
                        "The permission mode belongs to the agent's session rather than to \
                         this app, and an agent asks you in its own terminal, the way it does \
                         with no Splitlane around it \u{2014} nothing is held open and nothing is \
                         written to your settings files. Which session is waiting for you is \
                         still shown: that is read from the agent's own transcript and from \
                         whether its process is working, so the dot beside a session does not \
                         depend on a hook staying wired.",
                    ),
            )
    }

    /// One General-page choice row: the design's row anatomy, with the chip
    /// opening a menu of `options` - `(label, leading_logo, json_value,
    /// is_selected)`. Every field this drives is top-level, so the write is
    /// always un-nested.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn general_choice_row(
        &self,
        which: GeneralDropdown,
        label: &'static str,
        hint: Option<SharedString>,
        current_label: String,
        options: Vec<SelectOption>,
        config_key: &'static str,
        ui: crate::theme::UiColors,
        // Concrete `AnyElement` (not `impl IntoElement`) so the value does not
        // capture `cx`'s borrow under edition-2024 RPIT - otherwise two `let`
        // rows would hold overlapping `&mut cx` borrows.
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = self.general_dropdown == Some(which);

        let menu = is_open.then(|| {
            let mut menu = select_menu(
                SharedString::from(format!("general-dd-list-{config_key}")),
                ui,
            )
            // Guard on `which` so opening another select does not close it via
            // this menu's out-handler (shared state).
            .on_mouse_down_out(cx.listener(move |this, _, _w, cx| {
                if this.general_dropdown == Some(which) {
                    this.general_dropdown = None;
                    cx.notify();
                }
            }));
            for (i, (label, icon, value, selected)) in options.into_iter().enumerate() {
                let value_for_click = value;
                let mut item = select_item((config_key, i), selected, ui)
                    .cursor(CursorStyle::Arrow)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.general_dropdown = None;
                        this.persist_setting(false, config_key, value_for_click.clone(), cx);
                    }));
                if let Some(icon) = icon {
                    item = item.child(render_logo(icon, ui));
                }
                item = item.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(ui.text)
                        .child(label),
                );
                menu = menu.child(item);
            }
            menu.into_any_element()
        });

        setting_choice_row(
            ui,
            SharedString::from(format!("general-dd-{config_key}")),
            label,
            hint,
            value_chip(ui, current_label, ChipTone::Plain),
            menu,
            // Decide open/close from the render-time `is_open` snapshot, not
            // the live state: the menu's `on_mouse_down_out` fires on this same
            // press and may have already cleared it, so a live toggle re-opens.
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.general_dropdown = if is_open { None } else { Some(which) };
                this.settings_focus.focus(window, cx);
                cx.notify();
            }),
        )
        .into_any_element()
    }
}

/// `/Users/me/…` as `~/…`. The data directory is a fact worth stating and a
/// path worth shortening: the row is 620 wide and the home prefix is the part
/// nobody reads.
fn tilde(path: &str) -> String {
    match dirs::home_dir() {
        Some(home) => {
            let home = home.display().to_string();
            match path.strip_prefix(&home) {
                Some(rest) => format!("~{rest}"),
                None => path.to_string(),
            }
        }
        None => path.to_string(),
    }
}

/// Per-editor leading logo for the Default-editor select. Brand-color logos
/// (Zed / VS Code / Visual Studio) are PNGs rendered in full color; Cursor and
/// Windsurf ship as monochrome `currentColor` SVGs that follow the theme.
/// `auto` / `system` have no logo.
pub(crate) const EDITOR_PRESETS: &[(&str, &str)] = &[
    ("Auto-detect", "auto"),
    ("Zed", "zed"),
    ("Cursor", "cursor"),
    ("Windsurf", "windsurf"),
    ("VS Code", "code"),
    ("Visual Studio", "visual_studio"),
    ("System default", "system"),
];

pub(crate) fn editor_icon(value: &str) -> Option<Logo> {
    match value {
        "zed" => Some(("icons/editor-zed.png", true)),
        "code" => Some(("icons/editor-vscode.png", true)),
        "visual_studio" => Some(("icons/editor-visual-studio.png", true)),
        "cursor" => Some(("icons/editor-cursor.svg", false)),
        "windsurf" => Some(("icons/editor-windsurf.svg", false)),
        _ => None,
    }
}
