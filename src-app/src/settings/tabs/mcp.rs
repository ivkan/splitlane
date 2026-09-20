//! "MCP" settings page, in the design's row anatomy - it registers the embedded
//! `splitlane-mcp` bridge with every detected CLI agent (Claude Code, Codex,
//! Gemini, opencode) so they can read other panes' output.
//!
//! The design used to draw one row per **server**, closed by a dashed "+ add
//! server". It was replaced with ours: one built-in bridge, one row per **agent** it
//! is registered with, values `registered` / `needs repair` / `not registered`
//! over `stdio`, and no add affordance - there is no second server to add. The
//! page header is "MCP" now too, matching the nav; "MCP servers" promised a list
//! of servers that does not exist. The dashed row is the register/repair action,
//! which the design keeps ("repair re-registers it without touching the
//! sessions").
//!
//! The state (status snapshot, last install result, busy flag) is cached on
//! `SplitlaneApp` and refreshed off the GPUI main thread, so this render does
//! zero config I/O. Warmed when the page is opened
//! (`select_settings_section`) and after each install.

use gpui::{ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, px};

use splitlane_mcp_install::{InstallKind, OverallState, StatusKind};

use crate::SplitlaneApp;
use crate::settings::components::{
    ChipTone, SETTING_ROW_MAX_WIDTH, setting_action_row, setting_note, setting_row,
};
use crate::ui_tokens as tok;

impl SplitlaneApp {
    pub(crate) fn render_mcp_servers_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();

        let state = self
            .mcp_status
            .as_deref()
            .map(splitlane_mcp_install::overall_state);

        // The dashed row's label, and whether the press is live.
        let (label, enabled): (SharedString, bool) = if self.mcp_busy {
            ("Installing\u{2026}".into(), false)
        } else {
            match state {
                None => ("Checking\u{2026}".into(), false),
                Some(OverallState::NoAgents) => ("No agents found".into(), false),
                Some(OverallState::AllInstalled) => ("Re-register the bridge".into(), true),
                Some(OverallState::NeedsRepair) => ("Repair the bridge".into(), true),
                Some(OverallState::NeedsInstall) => ("+ register the bridge".into(), true),
            }
        };

        // One row per agent, in the design's row anatomy: the agent's name, the
        // detail in mono, and a status chip whose tone is the claim it makes.
        let mut rows = div().flex().flex_col();
        for row in self.mcp_rows() {
            rows = rows.child(setting_row(ui, row.label, row.hint, row.value, row.tone));
        }

        let error = self.mcp_install_error().map(|error| {
            div()
                .max_w(px(SETTING_ROW_MAX_WIDTH))
                .pt(tok::space::XL)
                .text_size(tok::text::CAPTION)
                .text_color(danger_color())
                .child(error)
        });

        div()
            .flex()
            .flex_col()
            .child(setting_note(
                ui,
                "One built-in bridge, registered per agent, over stdio. Splitlane lists the \
                 agents it is registered with; repair re-registers it without touching the \
                 sessions, and touches only the splitlane entry.",
            ))
            .child(rows)
            .child(setting_action_row(
                ui,
                "mcp-install",
                label,
                enabled,
                cx.listener(|this, _: &ClickEvent, _w, cx| {
                    this.start_mcp_install(cx);
                }),
            ))
            .children(error)
    }

    /// The refusal message from the last install, if it failed wholesale
    /// (bridge missing / data dir unresolved).
    fn mcp_install_error(&self) -> Option<SharedString> {
        match &self.mcp_install {
            Some(Err(msg)) => Some(SharedString::from(msg.clone())),
            _ => None,
        }
    }

    /// One row per detected agent. Uses the last install result when present,
    /// else the cached status snapshot.
    fn mcp_rows(&self) -> Vec<McpRow> {
        if let Some(Ok(results)) = &self.mcp_install {
            return results
                .iter()
                .map(|r| {
                    let (value, hint, tone) = match &r.kind {
                        InstallKind::Installed => ("registered", Some("stdio"), ChipTone::On),
                        InstallKind::Updated => ("registered", Some("stdio"), ChipTone::On),
                        InstallKind::AlreadyCurrent => (
                            "registered",
                            Some("stdio \u{00b7} already up to date"),
                            ChipTone::On,
                        ),
                        InstallKind::SkippedAbsent => ("not found", None, ChipTone::Off),
                        InstallKind::Error(e) => {
                            return McpRow {
                                label: SharedString::from(r.label.to_string()),
                                hint: Some(SharedString::from(e.to_string())),
                                value: "error".into(),
                                tone: ChipTone::Attention,
                            };
                        }
                    };
                    McpRow {
                        label: SharedString::from(r.label.to_string()),
                        hint: hint.map(SharedString::from),
                        value: value.into(),
                        tone,
                    }
                })
                .collect();
        }
        match &self.mcp_status {
            Some(statuses) => statuses
                .iter()
                .map(|r| {
                    let (value, hint, tone) = match &r.kind {
                        StatusKind::NotDetected => ("not found", None, ChipTone::Off),
                        StatusKind::Installed { .. } => ("registered", Some("stdio"), ChipTone::On),
                        StatusKind::Stale { .. } => (
                            "needs repair",
                            Some("stdio \u{00b7} stale path"),
                            ChipTone::Attention,
                        ),
                        StatusKind::NeedsRepair { .. } => {
                            ("needs repair", Some("stdio"), ChipTone::Attention)
                        }
                        StatusKind::NotInstalled => {
                            ("not registered", Some("stdio"), ChipTone::Off)
                        }
                        StatusKind::Error(e) => {
                            return McpRow {
                                label: SharedString::from(r.label.to_string()),
                                hint: Some(SharedString::from(e.to_string())),
                                value: "error".into(),
                                tone: ChipTone::Attention,
                            };
                        }
                    };
                    McpRow {
                        label: SharedString::from(r.label.to_string()),
                        hint: hint.map(SharedString::from),
                        value: value.into(),
                        tone,
                    }
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Refresh the cached MCP bridge status off the main thread. Reads each
    /// agent's config (no writes), then stores the snapshot + repaints. Called
    /// when the settings page opens and when the MCP page is selected.
    pub(crate) fn refresh_mcp_status(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let status = smol::unblock(|| {
                let bridge = crate::runtime_paths::bridge_binary_path();
                splitlane_mcp_install::status_all(bridge.as_deref())
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.mcp_status = Some(status);
                cx.notify();
            });
        })
        .detach();
    }

    /// Install the bridge into every detected agent, off the main thread.
    /// Extracts the bridge binary first (so the registered path exists), then
    /// runs the install + a fresh status probe, and stores both.
    fn start_mcp_install(&mut self, cx: &mut Context<Self>) {
        if self.mcp_busy {
            return;
        }
        self.mcp_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (install, status) = smol::unblock(|| {
                let bridge = match crate::ai_hooks::extract::ensure_bridge_extracted() {
                    Ok(p) => Some(p),
                    Err(e) => {
                        log::warn!(
                            "settings: MCP bridge extraction failed ({e:#}); install may refuse"
                        );
                        crate::runtime_paths::bridge_binary_path()
                    }
                };
                let install = splitlane_mcp_install::install_all(bridge.as_deref());
                let status = splitlane_mcp_install::status_all(bridge.as_deref());
                (install, status)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.mcp_busy = false;
                this.mcp_install = Some(install);
                this.mcp_status = Some(status);
                cx.notify();
            });
        })
        .detach();
    }
}

/// One agent's row on the MCP page.
struct McpRow {
    label: SharedString,
    hint: Option<SharedString>,
    value: SharedString,
    tone: ChipTone,
}

/// Error-text color for the MCP recap.
///
/// The comment that used to sit here said `UiColors` has no danger slot and
/// the settings chrome is theme-independent, and both halves are now false:
/// `agent_error` is the danger slot, and the settings pages are drawn from the
/// roles like everything else. The fixed One Dark red it justified was a pale
/// salmon on a white page.
fn danger_color() -> gpui::Hsla {
    crate::theme::ui_colors().agent_error
}
