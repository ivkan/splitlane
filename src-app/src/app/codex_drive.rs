//! "Let Codex drive sessions": the switch that lets a Codex session reach
//! this app, and the card that says what turning it on costs.
//!
//! Codex's sandbox refuses the app's socket, and the only way Codex offers to
//! allow it also gives the commands of a turn the network (see
//! `splitlane_mcp_install::codex_permissions`). So the switch is off until a
//! person turns it on, and turning it on stops at a card first: the cost is
//! not something to find out afterwards. The card takes no keyboard answer,
//! so `enter` held from elsewhere cannot agree to it.
//!
//! The profile lives in Codex's own config. The setting here records that
//! the person asked for it, so a later start can write it again if the
//! socket moved, and so turning it off knows there is something to remove.

use std::path::PathBuf;

use gpui::Context;
use splitlane_mcp_install::codex_permissions::{self, Refusal};

use crate::SplitlaneApp;

/// Only macOS: Codex allows a Unix socket by name on that platform alone.
pub(crate) const AVAILABLE: bool = cfg!(target_os = "macos");

fn config_path() -> Option<PathBuf> {
    codex_permissions::config_path(dirs::home_dir(), std::env::var_os("CODEX_HOME"))
}

fn socket() -> Option<String> {
    crate::runtime_paths::socket_path().map(|path| path.to_string_lossy().into_owned())
}

fn enable() -> Result<(), Refusal> {
    let (Some(path), Some(socket)) = (config_path(), socket()) else {
        return Err(Refusal::Io(
            "Codex's config or Splitlane's socket could not be located".to_string(),
        ));
    };
    codex_permissions::enable(&path, &socket)
}

fn disable() -> Result<(), Refusal> {
    match config_path() {
        Some(path) => codex_permissions::disable(&path),
        None => Ok(()),
    }
}

/// Write the profile again at start when the person has asked for it, so a
/// socket that moved is the one allowed.
///
/// A debug build writes nothing here: its socket is its own, and the Codex
/// config it would write to is the one an installed release shares.
pub(crate) fn keep_at_start(enabled: bool) {
    if !AVAILABLE || !enabled || cfg!(debug_assertions) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("codex-drive".into())
        .spawn(|| {
            if let Err(refusal) = enable() {
                log::warn!("codex drive: profile not written at start: {refusal}");
            }
        });
    if let Err(error) = spawned {
        log::warn!("codex drive: could not start the writer thread: {error}");
    }
}

/// The card's title, body and button.
pub(crate) fn confirm_copy() -> (String, String, String) {
    let config = config_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Codex's config.toml".to_string());
    (
        "Let Codex drive sessions?".to_string(),
        format!(
            "This gives every command Codex runs access to the internet, in every \
             project. Codex has no way to allow Splitlane without it.\n\n\
             Codex will also write inside a project's folder without asking first, \
             including a project it has not been told to trust.\n\n\
             Splitlane adds a profile named \"splitlane\" to {config} and makes it \
             Codex's default. Turning this off removes it. It applies to Codex \
             sessions started afterwards."
        ),
        "Allow".to_string(),
    )
}

impl SplitlaneApp {
    /// The switch was pressed.
    pub(crate) fn toggle_codex_drive(&mut self, cx: &mut Context<Self>) {
        self.codex_drive_note = None;
        if self.cached_config.codex_drive_enabled() {
            self.persist_setting(false, "codex_drive", serde_json::Value::Bool(false), cx);
            cx.spawn(async move |this, cx| {
                let result = smol::unblock(disable).await;
                let _ = this.update(cx, |this, cx| {
                    if let Err(refusal) = result {
                        this.codex_drive_note =
                            Some(format!("The profile was not removed: {refusal}."));
                    }
                    cx.notify();
                });
            })
            .detach();
        } else {
            self.codex_drive_confirm = true;
        }
        cx.notify();
    }

    pub(crate) fn cancel_codex_drive(&mut self, cx: &mut Context<Self>) {
        self.codex_drive_confirm = false;
        cx.notify();
    }

    /// The person read the card and agreed. The setting is recorded only once
    /// the profile is in Codex's config, so the switch never claims an access
    /// Codex does not have.
    pub(crate) fn confirm_codex_drive(&mut self, cx: &mut Context<Self>) {
        self.codex_drive_confirm = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(enable).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.persist_setting(
                            false,
                            "codex_drive",
                            serde_json::Value::Bool(true),
                            cx,
                        );
                        this.codex_drive_note = Some(
                            "On for Codex sessions started from now. A session already \
                             running keeps the sandbox it started with."
                                .to_string(),
                        );
                    }
                    Err(refusal) => {
                        this.codex_drive_note = Some(format!("Not turned on: {refusal}."));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_codex_drive_confirm(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (title, body, confirm_label) = confirm_copy();
        crate::app::confirm_dialog::render_confirm_dialog(
            crate::app::confirm_dialog::ConfirmDialog {
                id: "codex-drive-confirm",
                title,
                body,
                confirm_label,
                safe: None,
                focus: None,
                on_cancel: std::rc::Rc::new(|this, _window, cx| this.cancel_codex_drive(cx)),
                on_confirm: std::rc::Rc::new(|this, _window, cx| this.confirm_codex_drive(cx)),
            },
            ui,
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card is the only place the cost is stated before the write, so
    /// each part of it has to be there in words a person reads.
    #[test]
    fn the_card_names_the_network_the_writes_and_the_way_back() {
        let (title, body, confirm) = confirm_copy();
        assert!(title.ends_with('?'));
        assert!(body.contains("access to the internet"), "{body}");
        assert!(body.contains("without asking first"), "{body}");
        assert!(body.contains("Turning this off removes it"), "{body}");
        assert!(body.contains("started afterwards"), "{body}");
        assert_eq!(confirm, "Allow");
    }
}
