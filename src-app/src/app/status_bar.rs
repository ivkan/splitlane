//! The status bar: the 28px strip along the bottom of the window.
//!
//! One line of mono, and every item in it is a fact the app already holds:
//! the container's branch and diffstat, what the content area is showing,
//! the chord into the palette, what the last restore brought back, and how
//! much is open in total.
//!
//! The hint used to name `next_workspace`, which sat on a chord macOS gives to
//! its own application switcher. That chord is gone and the design has no
//! replacement for it: moving between projects is what the palette is for, so
//! the hint names the palette.
//!
//! **It degrades by width, and the order is the design's**: the focus label
//! shrinks with an ellipsis first, then the restore indicator drops, then the
//! palette hint. Branch, diff counts and the summary are never dropped.
//! The width it degrades by is the content area's (the window minus the rails)
//! because that is what actually changes when a panel opens, and it is
//! arithmetic here rather than a measured canvas: `render` already knows every
//! term of it.
//!
//! A container that is not a repository shows no branch and no counts, per the
//! design's degraded states. Nothing takes their place: an empty slot is the
//! statement.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, Window, deferred, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::theme::UiColors;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// Below this the restore indicator drops.
const DROP_RESTORE_BELOW: f32 = 820.;
/// Below this the palette hint drops too.
const DROP_HINT_BELOW: f32 = 760.;
/// The width of the popover that opens when a container is serving on more
/// than one port.
const PORTS_POPOVER_WIDTH: f32 = 220.;

/// What the last session restore brought back, for the design's
/// "restored 09:41 · 2 sessions".
#[derive(Clone, Copy, Debug)]
pub(crate) struct RestoreSummary {
    /// Unix millis of the restore.
    pub(crate) at_unix_ms: u64,
    /// Surfaces the restore brought back.
    pub(crate) surfaces: usize,
}

impl SplitlaneApp {
    pub(crate) fn render_status_bar(
        &self,
        content_width: f32,
        window: &Window,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let workspace = self.workspaces.get(self.active_idx);
        let has_repo = workspace.is_some_and(|ws| ws.repo_root.is_some());
        let branch = workspace
            .map(|ws| ws.git_branch.clone())
            .filter(|branch| !branch.is_empty());
        let stats = workspace.map(|ws| ws.git_stats.clone());
        let diff = ui.diff_colors();

        let mut row = div()
            .flex_none()
            .h(tok::row::PROJECT)
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XXL)
            .px(tok::space::XL)
            .overflow_hidden()
            .bg(ui.chrome)
            .border_t_1()
            .border_color(ui.divider)
            .font_family(tok::font::MONO)
            .text_size(tok::mono::LABEL)
            .text_color(ui.dim);

        // Branch and diffstat. A plain directory gets neither, and nothing
        // stands in for them.
        if has_repo {
            let mut git = div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::MD)
                .child(div().child(SharedString::from(
                    branch.unwrap_or_else(|| "detached".to_string()),
                )));
            if let Some(stats) = stats.filter(|stats| !stats.is_empty()) {
                git = git
                    .child(
                        div()
                            .text_color(diff.added)
                            .child(format!("+{}", stats.insertions)),
                    )
                    .child(
                        div()
                            .text_color(diff.deleted)
                            .child(format!("-{}", stats.deletions)),
                    );
            }
            row = row.child(git).child(separator(ui));
        }

        // What the content area is showing. First to shrink, never to drop.
        row = row.child(div().min_w_0().truncate().child(SharedString::from(format!(
            "focus: {}",
            self.focus_label(window, cx)
        ))));

        if content_width >= DROP_HINT_BELOW
            && let Some(chord) = self.shortcut_for_action("open_command_palette")
        {
            row = row.child(separator(ui)).child(
                div()
                    .flex_none()
                    .child(format!("{chord} go to project or session")),
            );
        }

        // The dev servers this container is running. A port is a fact that
        // keeps being true while the process lives, and facts that persist
        // belong in chrome rather than in a stream of events - which is why
        // this is here and not in the transcript.
        //
        // The chip sits before the spacer, next to the hint, exactly where the
        // mockup's markup puts it. It is never dropped by width: unlike the
        // hint and the restore indicator it is the only place in the app that
        // says a server is up, and it is at most a dozen mono characters.
        if let Some(ports) = self.status_bar_ports(ui, cx) {
            row = row.child(separator(ui)).child(ports);
        }

        row = row.child(div().flex_1().min_w_0());

        if content_width >= DROP_RESTORE_BELOW
            && let Some(restore) = self.restore_summary
        {
            row = row.child(
                div()
                    .flex_none()
                    .child(SharedString::from(restore_label(restore))),
            );
        }

        row.child(separator(ui))
            .child(
                div()
                    .flex_none()
                    .child(SharedString::from(self.open_summary(cx))),
            )
            .into_any_element()
    }

    /// The active container's reachable dev servers, newest chip first.
    ///
    /// Reachable means `is_frontend`: a port with no URL to open is a fact the
    /// container menu already lists, and a chip that does nothing when clicked
    /// is worse than no chip.
    fn frontend_services(&self) -> Vec<(u16, String)> {
        let Some(workspace) = self.workspaces.get(self.active_idx) else {
            return Vec::new();
        };
        let mut services: Vec<(u16, String)> = workspace
            .active_ports
            .iter()
            .filter_map(|port| {
                let info = workspace.service_labels.get(port)?;
                if !info.is_frontend {
                    return None;
                }
                let url = info
                    .url
                    .clone()
                    .unwrap_or_else(|| format!("http://localhost:{port}"));
                Some((*port, url))
            })
            .collect();
        services.sort_by_key(|(port, _)| *port);
        services.dedup_by_key(|(port, _)| *port);
        services
    }

    /// The port chip, and the popover behind it when there is more than one.
    ///
    /// One server is named outright - `localhost:5173 \u{2197}` - because that is
    /// the whole fact and clicking it is the whole action. Several become a
    /// count that opens a list of the same rows: the status bar is one line
    /// that already degrades by width, and a chip per server would push the
    /// focus label out to say something the list says better.
    fn status_bar_ports(&self, ui: UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let services = self.frontend_services();
        let (first_port, first_url) = services.first()?.clone();
        let several = services.len() > 1;
        let label = ports_chip_label(services.len(), first_port);

        let chip = div()
            .id("status-bar-ports")
            .flex_none()
            .cursor_pointer()
            .text_color(ui.accent)
            .whitespace_nowrap()
            .animated_hover(move |style, delta| {
                style.text_color(lerp_color(ui.accent, ui.text, delta));
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                if several {
                    this.ports_popover_open = !this.ports_popover_open;
                    cx.notify();
                } else {
                    this.open_workspace_service_url(&first_url, cx);
                }
                cx.stop_propagation();
            }))
            .child(SharedString::from(label));

        if !several || !self.ports_popover_open {
            return Some(chip.into_any_element());
        }

        // Anchored to the chip rather than to the window: the chip's x depends
        // on how much the focus label took, which is not known until layout.
        let mut list = div()
            .id("status-bar-ports-popover")
            .occlude()
            .absolute()
            .bottom(tok::row::PROJECT)
            .left_0()
            .w(px(PORTS_POPOVER_WIDTH))
            .flex()
            .flex_col()
            .p(tok::space::SM)
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .rounded(tok::radius::MENU)
            .shadow(crate::ui_primitives::menu_shadow(ui))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.ports_popover_open = false;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (port, url) in services {
            list = list.child(
                div()
                    .id(SharedString::from(format!("status-bar-port-{port}")))
                    .px(tok::space::MD)
                    .py(tok::space::XS)
                    .rounded(tok::radius::CONTROL)
                    .cursor_pointer()
                    .text_color(ui.accent)
                    .whitespace_nowrap()
                    .animated_hover(move |style, delta| {
                        style.bg(lerp_color(ui.subtle.opacity(0.), ui.subtle, delta));
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.ports_popover_open = false;
                        this.open_workspace_service_url(&url, cx);
                        cx.stop_propagation();
                    }))
                    .child(SharedString::from(format!("localhost:{port} \u{2197}"))),
            );
        }

        Some(
            div()
                .flex_none()
                .relative()
                .child(chip)
                .child(deferred(list).with_priority(5))
                .into_any_element(),
        )
    }

    /// The design's focus label, naming the kind of thing on screen and then
    /// the thing itself. `-` means the container has nothing open.
    ///
    /// One question now, where it used to be three: what is in the focused
    /// pane. The selection arms - a parked agent surface, the full-area diff -
    /// went with the second world they described.
    fn focus_label(&self, window: &Window, cx: &Context<Self>) -> String {
        let dash = "\u{2014}".to_string();
        let Some(pane) = self.focused_pane_as_shown(window, cx) else {
            return dash;
        };
        // Asked of `pane_kind`, which is the one home for "what is this pane
        // showing". It used to be asked here as `agent_thread_id.is_some()`,
        // which is "does this surface have a rail row" - true of a container's
        // shells as well, so a shell read as `agent` the moment it got a row.
        // `pane_kind` asks the rail's own question (`is_shell_surface`), so
        // the two cannot disagree.
        let kind = match self.pane_kind(&pane, cx) {
            Some(crate::app::targeting::SurfaceKind::Diff) => "review",
            Some(crate::app::targeting::SurfaceKind::Markdown) => "view",
            Some(crate::app::targeting::SurfaceKind::Agent) => "agent",
            Some(crate::app::targeting::SurfaceKind::Shell) => "shell",
            // An empty pane is a pane, and what it is showing has a name.
            None => return "launcher".to_string(),
        };
        let pane = pane.read(cx);
        if pane.showing_launcher() {
            return "launcher".to_string();
        }
        format!("{kind} \u{b7} {}", pane.active_tab_label(cx))
    }

    /// "2 agents · 4 shells · 4 projects" - the whole window, not the
    /// container, which is what makes it worth a permanent slot.
    fn open_summary(&self, cx: &Context<Self>) -> String {
        let mut agents = 0usize;
        let mut shells = 0usize;
        for ws in &self.workspaces {
            for thread in &ws.threads {
                if crate::app::agents_sidebar::is_shell_surface(thread) {
                    shells += 1;
                } else {
                    agents += 1;
                }
            }
            // Terminals living in the slot tree. A slot showing an agent
            // surface is skipped: its record is already counted above, and
            // counting the slot too would count one surface twice. A slot with
            // no terminal at all - a diff, a document, the launcher - is not a
            // shell either.
            if let Some(root) = &ws.root {
                shells += root
                    .collect_leaves()
                    .into_iter()
                    .filter(|pane| {
                        pane.read(cx)
                            .active_terminal_opt()
                            .is_some_and(|view| view.read(cx).agent_thread_id.is_none())
                    })
                    .count();
            }
        }
        format!(
            "{} \u{b7} {} \u{b7} {}",
            plural(agents, "agent"),
            plural(shells, "shell"),
            plural(self.workspaces.len(), "project"),
        )
    }
}

fn separator(ui: UiColors) -> AnyElement {
    div()
        .flex_none()
        .text_color(ui.faint)
        .child("|")
        .into_any_element()
}

/// What the port chip says: the one server by name, or how many there are.
/// Pure - unit-tested.
///
/// The count is what the chip carries above one, because the whole point of
/// naming the port is that it is the address you are about to open, and a
/// chip that names one of three would be naming the wrong one two thirds of
/// the time.
///
/// Only the single-server form takes the `\u{2197}`. The arrow says "this
/// leaves the app", and it is the rows of the list that do, not the count
/// that opens it.
fn ports_chip_label(count: usize, first_port: u16) -> String {
    if count > 1 {
        format!("{count} ports")
    } else {
        format!("localhost:{first_port} \u{2197}")
    }
}

/// "restored 09:41 · 2 sessions". Pure.
fn restore_label(restore: RestoreSummary) -> String {
    format!(
        "restored {} \u{b7} {}",
        crate::app::agents_sidebar::clock_hhmm(restore.at_unix_ms),
        plural(restore.surfaces, "session"),
    )
}

/// `1 agent` / `2 agents`. Pure - the plural is the only thing that can be
/// wrong in a counter.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// How wide the content area is: the window minus whichever rails are up.
/// Below the two thresholds the status bar starts dropping items.
pub(crate) fn content_area_width(window: &Window, left_rail: f32, right_rail: f32) -> f32 {
    (f32::from(window.viewport_size().width) - left_rail - right_rail).max(0.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_get_their_plurals_right() {
        assert_eq!(plural(0, "agent"), "0 agents");
        assert_eq!(plural(1, "agent"), "1 agent");
        assert_eq!(plural(2, "shell"), "2 shells");
    }

    #[test]
    fn the_port_chip_names_one_server_and_counts_the_rest() {
        assert_eq!(ports_chip_label(1, 5173), "localhost:5173 \u{2197}");
        assert_eq!(ports_chip_label(3, 5173), "3 ports");
        // Zero never reaches the chip - `status_bar_ports` returns early on an
        // empty list - but the label must not invent a server if it ever does.
        assert_eq!(ports_chip_label(0, 5173), "localhost:5173 \u{2197}");
    }

    #[test]
    fn the_restore_line_names_the_time_and_the_count() {
        // 08:12 local on the day of the epoch, whatever the machine's zone.
        let at = ((8 * 3600 + 12 * 60) as u64) * 1000;
        let label = restore_label(RestoreSummary {
            at_unix_ms: at,
            surfaces: 2,
        });
        assert!(label.starts_with("restored "), "{label}");
        assert!(label.ends_with(" \u{b7} 2 sessions"), "{label}");
    }

    /// The thresholds are ordered, so there is a band where the restore
    /// indicator is gone and the hint is still there - not one where both go
    /// at once.
    const _ORDERED: () = assert!(DROP_HINT_BELOW < DROP_RESTORE_BELOW);
}
