//! "Agents" settings page, in the design's row anatomy.
//!
//! The design gives this section four things: the command per agent, the detected
//! version, a "default" radio, and "+ add agent". The audit against it:
//!
//! - **The command per agent** was missing entirely and is now the row's mono
//!   hint, rendered from the same [`TerminalAgent::command`] the launcher runs -
//!   so what the row shows is what will be executed, not a copy of it.
//! - **One row per agent**, and the chip is the switch that was already there.
//!   The old copy said "Show the … launcher button in every tab bar", and there
//!   has been no tab-bar button cluster since the pane header was cleared: the
//!   gate now decides who appears in an empty pane's launcher. The rows read
//!   their key through [`TerminalAgent::visibility_key`] rather than a
//!   sixteen-entry table beside the sixteen-arm match that reads it back.
//! - **The detected version**, the **default radio** and **"+ add agent"** are
//!   gone from the design: all three were struck as prototype filler, and the
//!   design settles the status wording on "on PATH" / "not found", which is the honest
//!   limit of what we know without asking a CLI anything. The list stays closed:
//!   ours is an enum of sixteen where the design drew four, and "closed" was
//!   the point, not the number.
//!
//! Below the agents are the three switches that are about what an agent is
//! allowed to do rather than which agent runs. Their copy used to be a
//! paragraph inside the row; a row is one line, so the paragraph became the
//! caption under the group, which is where the reasoning for a dangerous switch
//! belongs - beside all of them, once.
//!
//! Persistence goes through [`SplitlaneApp::persist_setting`] (top level) and
//! [`SplitlaneApp::persist_agent_panel_setting`] (the `agent_panel` block): both
//! mutate the cached config for instant feedback and write `splitlane.json` off
//! the main thread. The MCP bridge installer lives on its own page
//! (`settings::tabs::mcp`).

use gpui::{ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, px};

use crate::SplitlaneApp;
use crate::agent_launcher::{PreferredAgent, TerminalAgent};
use crate::settings::components::{
    SETTING_ROW_MAX_WIDTH, section_header, setting_note, setting_toggle_row,
    setting_toggle_row_marked,
};
use crate::ui_tokens as tok;

impl SplitlaneApp {
    pub(crate) fn render_ai_agent_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Read the cached config (no per-frame `load_config()`).
        let config = &self.cached_config;
        let ui = crate::theme::ui_colors();

        // Effective state, not the raw key: an absent key defaults to
        // "offered only if the agent's CLI is installed" (see
        // `TerminalAgent::is_visible`). Toggling writes an explicit `Some(..)`
        // that pins the choice regardless of install state.
        let bypass = config.claude_code_bypass_permissions.unwrap_or(false);
        // AI free-access mode + the
        // independent injection fence. Defaults: unrestricted OFF, fence ON.
        let unrestricted = config.ai_unrestricted_enabled();
        let fence = config.ai_injection_fence_enabled();

        let mut agents = div().flex().flex_col();
        for agent in TerminalAgent::ALL {
            let offered = agent.is_visible(config);
            let key = agent.visibility_key();
            // The design's wording, and the honest limit of what we know: we
            // look the binary up on PATH and never ask a CLI for its version.
            let mut hint = agent.command(config);
            hint.push_str(if agent.is_installed() {
                " \u{00b7} on PATH"
            } else {
                " \u{00b7} not found"
            });
            agents = agents.child(setting_toggle_row_marked(
                ui,
                SharedString::from(format!("agent-{}", agent.tag())),
                agent.display_name(),
                crate::ui_primitives::capability_word_chip(agent, ui),
                Some(SharedString::from(hint)),
                offered,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(false, key, serde_json::Value::Bool(!offered), cx);
                }),
            ));
        }

        let mut access = div()
            .flex()
            .flex_col()
            .child(setting_toggle_row(
                ui,
                "agents-bypass",
                "Bypass permissions",
                Some(SharedString::from("--permission-mode bypassPermissions")),
                bypass,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(
                        false,
                        "claude_code_bypass_permissions",
                        serde_json::Value::Bool(!bypass),
                        cx,
                    );
                }),
            ))
            .child(setting_toggle_row(
                ui,
                "agents-free-access",
                "Free access",
                Some(SharedString::from(
                    "without the SPLITLANE_IPC_SCRIPTING gate",
                )),
                unrestricted,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(
                        false,
                        "ai_unrestricted",
                        serde_json::Value::Bool(!unrestricted),
                        cx,
                    );
                }),
            ));

        // The fence sub-row only appears once free access is on: with the mode
        // off, surface.read is always fenced and there is nothing to relax.
        if unrestricted {
            access = access.child(setting_toggle_row(
                ui,
                "agents-injection-fence",
                "Injection fence",
                Some(SharedString::from("a peer pane's output stays untrusted")),
                fence,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(
                        false,
                        "ai_injection_fence",
                        serde_json::Value::Bool(!fence),
                        cx,
                    );
                }),
            ));
        }

        access = access.child(caption(
            ui,
            "Bypass permissions launches Claude Code with no protection against prompt \
             injection \u{2014} only on machines you trust. Free access lets a lead agent \
             auto-submit prompts to your other panes: best on a throwaway branch, and every \
             write it makes is logged. The fence keeps what a lead agent reads out of a peer \
             pane wrapped as untrusted; it protects the agent rather than restricting it, \
             which is why it stays on even here.",
        ));

        // AC #3: once the fence is OFF, surface the active risk in the danger
        // role so the trade-off is explicit and impossible to miss.
        if unrestricted && !fence {
            access = access.child(
                div()
                    .max_w(px(SETTING_ROW_MAX_WIDTH))
                    .pt(tok::space::MD)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.agent_error)
                    .child(
                        "Fence disabled: a malicious pane can redirect your lead agent, and \
                         resuming control by hand will not undo a fast, silent injection.",
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .child(setting_note(
                ui,
                "A closed list \u{2014} Splitlane speaks these and does not ask them for a \
                 version. Commands run in the pane's directory; an agent session resumes on \
                 the next launch, a shell does not.",
            ))
            .child(capability_ladder(ui))
            .child(self.default_agent_row(ui, cx))
            .child(agents)
            .child(
                div()
                    .pt(tok::space::BLOCK)
                    .child(section_header(ui, "What an agent may do")),
            )
            .child(access)
    }
}

impl SplitlaneApp {
    /// Which agent the new-agent chord starts in a project that has never been
    /// asked.
    ///
    /// The **fallback**, and it says so: every project can answer for itself in
    /// its own rail-row menu, and this is what an unanswered one inherits. It
    /// exists because "always ask" cannot be the shipped default - a person
    /// with one agent installed should not answer a question every time - and
    /// because a per-project value with nothing under it leaves a fresh project
    /// with no answer at all.
    ///
    /// Only agents on PATH: naming one that cannot start is a promise the row
    /// cannot keep.
    fn default_agent_row(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let current = self.app_default_agent();
        let chord = self
            .shortcut_for_action("new_agent")
            .map(|chord| chord.to_string())
            .unwrap_or_else(|| "New agent".to_string());
        let mut options: Vec<crate::settings::tabs::general::SelectOption> = TerminalAgent::ALL
            .iter()
            .copied()
            .filter(|agent| agent.is_visible(&self.cached_config) && agent.is_installed())
            .map(|agent| {
                (
                    agent.display_name().to_string(),
                    None,
                    serde_json::Value::String(agent.tag().to_string()),
                    current == Some(PreferredAgent::Agent(agent)),
                )
            })
            .collect();
        options.push((
            "Always ask".to_string(),
            None,
            serde_json::Value::String(PreferredAgent::Ask.tag().to_string()),
            current.is_none() || current == Some(PreferredAgent::Ask),
        ));
        let current_label = match current {
            Some(PreferredAgent::Agent(agent)) => agent.display_name().to_string(),
            _ => "Always ask".to_string(),
        };
        self.general_choice_row(
            crate::GeneralDropdown::DefaultAgent,
            "New agent starts",
            Some(SharedString::from(format!(
                "{chord}, in a project that has not chosen its own"
            ))),
            current_label,
            options,
            "default_agent",
            ui,
            cx,
        )
    }
}

/// The agents matching a predicate, by name.
fn named(f: fn(TerminalAgent) -> bool) -> String {
    joined(f, |a| a.display_name().to_string())
}

fn joined(f: fn(TerminalAgent) -> bool, name: fn(TerminalAgent) -> String) -> String {
    TerminalAgent::ALL
        .iter()
        .copied()
        .filter(|a| f(*a))
        .map(name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The ladder, once, above the list - what the app can do and for how many.
///
/// # Why the numbers are counted and not written down
///
/// They are claims about this build, and the design's own figures (16 / 9 / 3 /
/// 1 / 2) are one release out of date on two rows: every agent whose sessions
/// the app reads, it can also reopen by id - nine of them, not three. A number
/// typed into a table is a number nothing keeps true, so each row counts
/// `TerminalAgent::ALL` through the same predicate the rest of the app decides
/// by.
///
/// The capability chip's rule is the table's too: a rung the app cannot
/// reach has no business looking like an achievement. There used to be a sixth
/// row - "Answer a permission ask here, rather than in its terminal" - and it
/// went with the permission bar rather than being restated as an off switch.
///
/// # Why the accent sits where it does
///
/// It used to sit on the bottom two rows, which were the two narrowest things
/// in the table - one agent and two. So the ladder sold the app on what it does
/// for two agents out of sixteen, and the row that is true of **all** of them
/// was drawn as the least interesting line on the page.
///
/// The accent is on what the app does for everyone and on what it rests on
/// instead of intercepting: running any of the sixteen in a pane beside the
/// others, and saying which session is waiting for a person.
///
/// # Why the last two rows name their members
///
/// Because at one, the count is not the answer - *which* is.
fn capability_ladder(ui: crate::theme::UiColors) -> impl IntoElement {
    let total = TerminalAgent::ALL.len();
    let count =
        |f: fn(TerminalAgent) -> bool| TerminalAgent::ALL.iter().copied().filter(|a| f(*a)).count();
    let rows: [(&str, usize, bool, Option<String>); 5] = [
        ("Run it in a pane, beside the others", total, false, None),
        (
            "List its past sessions",
            count(|a| a.session_agent().is_some()),
            false,
            None,
        ),
        (
            "Reopen one session by name",
            count(TerminalAgent::can_resume_by_name),
            false,
            None,
        ),
        // Ordered from the common to the rare, and the accent sits on the last
        // row for that reason - the rarest thing is the one worth pointing at,
        // and a reader who has got that far has already read the rest.
        //
        // Two things moved. The order: this row counted **two**
        // once Codex earned a reader, so it now sorts above "follow a session",
        // which counts one - it used to sit below it and broke the run. And the
        // accent: it used to be on **two** rows, this one and "run it in a
        // pane", which is the whole table's premise rather than a rung. One
        // accent, on the rarest.
        (
            "Tell you when it is waiting for you",
            count(TerminalAgent::reports_state),
            false,
            Some(named(TerminalAgent::reports_state)),
        ),
        (
            "Follow a session for certain, across restarts",
            count(TerminalAgent::supports_forced_session_id),
            true,
            Some(named(TerminalAgent::supports_forced_session_id)),
        ),
    ];
    let mut table = div().flex().flex_col().max_w(px(SETTING_ROW_MAX_WIDTH));
    for (label, n, advertised, members) in rows {
        table = table.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::LG)
                .h(tok::row::SESSION)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(tok::text::CAPTION)
                        .text_color(if advertised {
                            ui.accent
                        } else {
                            ui.text_secondary
                        })
                        .child(label),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::HINT)
                        .text_color(if advertised {
                            ui.accent
                        } else {
                            ui.text_tertiary
                        })
                        .child(SharedString::from(match members {
                            Some(members) if !members.is_empty() => {
                                format!("{n} of {total} \u{b7} {members}")
                            }
                            _ => format!("{n} of {total}"),
                        })),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .pb(tok::space::XL)
        .child(table)
        .child(caption(
            ui,
            "Every agent carries one of four words \u{2014} full control, resume by name, \
             history only, launch only \u{2014} and it is beside its name wherever an agent \
             is chosen. \"Waiting for you\" is read from the agent's own transcript and from \
             whether its process is working, so it needs nothing wired and nothing approved. \
             Permission asks are answered where the agent asks them: in its own terminal, \
             the way it would with no Splitlane around it.",
        ))
}

/// The paragraph a row cannot hold, under the group it belongs to. Same
/// treatment as the closing note in Limits and General.
fn caption(ui: crate::theme::UiColors, text: &'static str) -> impl IntoElement {
    div()
        .max_w(px(SETTING_ROW_MAX_WIDTH))
        .pt(tok::space::XL)
        .text_size(tok::text::CAPTION)
        .text_color(ui.text_tertiary)
        .child(text)
}
