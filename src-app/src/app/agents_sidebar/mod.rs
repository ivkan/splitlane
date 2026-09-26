//! The rail: one list of containers, each expanding to its surfaces.
//!
//! There used to be three of these - the CLI workspace list, the Agents
//! project/thread list, and the Diff file list - each the arm of an
//! `AppMode` branch. This is what they merged into: container rows with one
//! field set, and under each of them its surfaces (`Changes`, the slot tree's
//! terminals, the parked agent threads). The Diff rail's file list moved
//! inside the diff surface, where it belongs to the thing it describes.
//!
//! The sidebar is the only surface the user has for
//! cross-thread navigation; it must stay responsive at 60 fps with up
//! to 100 threads per project (5 000 threads total with a database-backed
//! query). We lean on the same `overflow_y_scroll +
//! sidebar_list_wrapper` pattern the workspace sidebar uses (200 rows
//! of plain `div` render comfortably under 16 ms on a mid-range
//! laptop, verified manually). When real-world data sizes exceed that
//! envelope, switching to `gpui::list` is a localised change inside
//! [`Self::render_agents_sidebar`] -- no consumer touches the row
//! widgets directly.

mod affordances;
mod context_menus;
mod create;
mod groups;
pub(crate) mod limits_footer;
// Shared with the sessions sidebar (`app::sessions_sidebar`), which filters its
// rows with the same matcher so both search fields behave identically.
pub(crate) mod filter;
mod header;
mod state;

pub(crate) use context_menus::render_open_agents_menu;
pub(crate) use limits_footer::{
    ClaudeLimits, LIMITS_FIRST_READ_DELAY, LIMITS_TICK_CHECK, LimitsRefresh, clock_hhmm,
};
pub(crate) use state::{AgentsContextMenu, AgentsDeleteTarget, AgentsRenameTarget, MenuOrigin};

use gpui::{
    Animation, AnimationExt, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Role, SharedString, StatefulInteractiveElement, Styled, Transformation, div,
    percentage, prelude::*, px, svg,
};

use crate::SplitlaneApp;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};

use crate::ui_tokens as tok;

/// Horizontal padding inside a rail row, container and surface alike.
const RAIL_ROW_PADDING_X: gpui::Pixels = tok::space::MD;
/// Gap between a row's parts: caret to name, glyph to name, name to badge.
const RAIL_ROW_GAP: gpui::Pixels = tok::space::MD;
/// Width of the container row's disclosure caret box.
const RAIL_CARET_WIDTH: gpui::Pixels = tok::space::LG;
/// How far a surface row hangs inside its container row's left edge, and how
/// far its own content then starts from there. The design states 10 + 20; the
/// second step rounds to the spacing scale's neighbouring 18 (P0's rule: the
/// stated scale wins over the prototype's stray values).
const RAIL_SURFACE_MARGIN_L: gpui::Pixels = tok::space::LG;
const RAIL_SURFACE_INDENT: gpui::Pixels = tok::space::SECTION;
/// The status dot at the right edge of a surface row.
const RAIL_STATUS_DOT: f32 = 6.0;

/// Emit a `tracing::debug!` when
/// [`SplitlaneApp::render_agents_sidebar`] exceeds the 16 ms frame
/// budget. We intentionally do NOT debounce filter keystrokes (the
/// lowercase-once fix moved per-keystroke cost well below the 16 ms
/// frame budget; VSCode #6899 and Slack's Quick Switcher both
/// document the same call). This guard is the early-warning if a
/// future regression invalidates that assumption.
///
/// Threshold is 16 ms (the actual single-frame drop boundary at
/// 60 Hz) instead of a safety-margin 12 ms: a 13-15 ms render
/// still hits the frame and is uninteresting; only past 16 ms does
/// the user see a dropped frame. Level is `debug!` instead of
/// `warn!` so the line stays out of `splitlane-debug.log` at
/// `info` level (which is the user-facing default per
/// `main.rs::env_logger`) -- enable `RUST_LOG=splitlane_app::
/// agents_sidebar=debug` to surface it on demand.
struct RenderTimeCanary {
    start: std::time::Instant,
    project_count: usize,
}

impl RenderTimeCanary {
    fn new(project_count: usize) -> Self {
        Self {
            start: std::time::Instant::now(),
            project_count,
        }
    }
}

impl Drop for RenderTimeCanary {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        if elapsed > std::time::Duration::from_millis(16) {
            tracing::debug!(
                target: "splitlane_app::agents_sidebar",
                "render_rail exceeded 16ms frame budget: {:.2}ms across {} projects -- chose no-debounce on the bet that the work stays sub-frame. If this fires repeatedly, profile and consider a 50ms input debounce.",
                elapsed.as_secs_f64() * 1000.0,
                self.project_count,
            );
        }
    }
}

impl SplitlaneApp {
    /// Render the rail: header (one create button), search, then the PINNED /
    /// PROJECTS sections, then the footer entry to the application layer.
    ///
    /// Data binds directly from `self.workspaces`. Newest agent
    /// surfaces appear first (we iterate in reverse, since
    /// [`crate::project::next_thread_id`] is monotonic so insertion order
    /// tracks `created_at`). Empty sections hide their eyebrow rather than
    /// leave an orphan label.
    pub(crate) fn render_rail(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // Render-time canary. The original requirement
        // proposed a 50ms keystroke debounce, but the lowercase-once
        // fix moved per-keystroke cost well under the 16ms frame
        // budget so debounce was skipped on VSCode/Slack precedent.
        // This Drop-based guard emits a debug-level log if the
        // 16 ms frame budget is ever exceeded -- an early-warning
        // canary so a future regression (added complexity, larger
        // thread count, slower filter impl) gets noticed under a
        // targeted `RUST_LOG=...=debug` profiling session, without
        // spamming `splitlane-debug.log` for every user on slower
        // hardware.
        let _render_canary = RenderTimeCanary::new(self.workspaces.len());
        let ui = crate::theme::ui_colors();

        let mut sidebar = div()
            .relative()
            // The width the window gives it, not the default it starts at.
            // This used to be `RAIL_WIDTH` flat, so dragging the rail's edge
            // moved the column and left its contents at 272: wider, and the
            // rows stopped short of the new edge; narrower, and they were
            // clipped by the column's `overflow_hidden`.
            .w_full()
            .flex_shrink_0()
            .h_full()
            // No background of its own. The rail is drawn *over* the card - an
            // absolutely positioned surface, inset 4px, rounded, bordered - and
            // a second fill here is a square rectangle painted over that
            // rounding, which is why the card's corners had notches and its
            // border went missing along the sides. Two backgrounds for one
            // surface is the bug; in the dark theme `chrome` and `surface` are
            // close enough to hide it, and the light theme shows it plainly.
            .flex()
            .flex_col();

        sidebar = sidebar.child(self.render_rail_search(ui, cx));

        // The one section label, fixed above the scroll area with its own
        // create button: `+` at the root of the rail has exactly one answer,
        // and it is a container.
        sidebar = sidebar.child(section_eyebrow(
            "PROJECTS",
            Some(SharedString::from("rail-add-project")),
            ui,
            cx,
        ));

        // -- Scrollable list area. The wheel-scroll behaviour comes
        // from `overflow_y_scroll + track_scroll`; the visible scroll
        // bar has been removed, so the list uses the full sidebar
        // width and there is no trailing gutter.
        //
        // The rows go into a column of their own that does not shrink, and
        // that column is the scroller's only child. A scroller's items have no
        // automatic minimum height, and every item shrinks by default, so rows
        // placed straight into it gave up height to fit instead of overflowing:
        // the longer the list, the shorter every row, while `tok::row::SESSION`
        // said otherwise.
        let mut list = div().flex_none().w_full().flex().flex_col();

        let renaming = self.agents_view.agents_renaming;
        let rename_input = self.agents_view.agents_rename_input.clone();
        let shared = RowSharedState {
            focus: self.focused_surface(window, cx),
            panes_on_screen: self.panes_surface_visible().then_some(self.active_idx),
            renaming,
            rename_input,
            ui,
        };

        if self.workspaces.is_empty() {
            list = list.child(projects_empty_hint(ui));
        }
        // Worktree grouping: sibling checkouts of one repo stay adjacent. The
        // CLI rail computed this order; it is the one list's order now.
        let signature = Self::sidebar_order_signature(&self.workspaces);
        if self.sidebar_order_cache.borrow().signature != Some(signature) {
            let order = Self::compute_display_order(&self.workspaces);
            let mut cache = self.sidebar_order_cache.borrow_mut();
            cache.order = order;
            cache.signature = Some(signature);
        }
        let display_order = self.sidebar_order_cache.borrow().order.clone();
        // Projects without a group first, under `PROJECTS`, then each group.
        // With no group anywhere this is the loop the rail always had.
        let sections = crate::app::project_groups::rail_sections(
            &display_order,
            |index| self.workspaces.get(index).and_then(|ws| ws.group),
            &self.project_groups,
        );
        for (position, &ws_idx) in sections.ungrouped.iter().enumerate() {
            list = self.project_block(list, ws_idx, position > 0, &shared, cx);
        }
        list = self.render_group_sections(list, &sections, &shared, window, cx);

        let list = div()
            .id("agents-sidebar-list")
            .flex_1()
            .min_w_0()
            .overflow_x_hidden()
            .overflow_y_scroll()
            .track_scroll(&self.sidebar_scroll)
            .flex()
            .flex_col()
            // The edge every indent below is measured from, and the inset that
            // keeps a selected row's fill off the rail's own border.
            .px(tok::space::MD)
            .pb(tok::space::MD)
            .child(list);
        sidebar = sidebar.child(self.sidebar_list_wrapper(list, cx));
        for banner in self.render_sidebar_banners(cx) {
            sidebar = sidebar.child(banner);
        }
        // One panel at the foot of the rail: the limits and, under a rule
        // inside it, the door to the application layer.
        //
        // It is inset by the card's own inset plus its border, and its bottom
        // corners follow the card's. The rail is drawn *over* an absolutely
        // positioned card - 4px in on every side, 1px border, rounded - so a
        // background that spans the rail's full width paints over that border
        // and squares off those corners. That is the overlap the design never
        // has: the design's sidebar is a column with one right border, and
        // full-bleed rules were drawn for it.
        // The card's own 1px border, named because two different offsets are
        // built from it below and conflating them is what broke the corner.
        let card_border = px(1.);
        // Distance from the **rail column's** edge: the card is inset by
        // `SIDEBAR_CARD_INSET`, and its border takes one pixel more.
        let card_edge = px(crate::app::constants::SIDEBAR_CARD_INSET) + card_border;
        sidebar = sidebar.child(
            div()
                .flex_none()
                .mx(card_edge)
                .mb(card_edge)
                .px(tok::space::XXL)
                // Under the application row, not around it: the rule above it
                // is the panel's own, and a row flush against the panel's edge
                // reads as a row that fell out of it.
                .pb(tok::space::XS)
                // The panel's own top inset, held here rather than by the
                // limits block, because the limits block is not always drawn -
                // see `limits_footer_has_something_to_say`.
                .pt(tok::space::XL)
                // Concentric with the card, so the two arcs do not cross.
                //
                // The offset that matters here is from the **card's** edge, not
                // from the rail column's: this panel sits exactly one pixel
                // inside the card, on the inner side of its border. It used to
                // subtract `card_edge` (5) instead of that one pixel, giving a
                // radius of 7 against the card's 12 - a far tighter arc that cut
                // across the card's own, and the card's bottom-left border
                // simply stopped in mid-air. Reported from a screenshot and
                // mapped on Harbor Light, 27 August 2026.
                .rounded_b(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS - card_border)
                .border_t_1()
                .border_color(ui.divider)
                .bg(ui.chrome)
                .child(self.render_rail_limits_footer(ui, window, cx))
                .child(self.render_sidebar_settings_footer(self.rail_menu_items(), cx)),
        );
        sidebar.into_any_element()
    }

    /// One project block: the project's row and, when it is open, everything
    /// under it. The same block is drawn under `PROJECTS` and inside a group.
    fn project_block(
        &self,
        mut list: gpui::Div,
        ws_idx: usize,
        lead_gap: bool,
        shared: &RowSharedState,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let renaming = shared.renaming;
        let ui = shared.ui;
        let project = &self.workspaces[ws_idx];
        let project_id = project.id;
        let is_expanded = project.is_expanded;
        let title = project.title.clone();
        let is_renaming_project =
            matches!(renaming, Some(AgentsRenameTarget::Project { ws_idx: r }) if r == ws_idx);

        list = list.child(self.project_header_row(
            ProjectHeaderArgs {
                ws_idx,
                project_id,
                title,
                is_expanded,
                rename_input: if is_renaming_project {
                    shared.rename_input.clone()
                } else {
                    None
                },
                surface_count: self.container_surface_count(ws_idx, cx),
                // One word per session, its highest - see
                // `FoldedTally::of`. A folded project leaves `waiting`
                // to the title bar's chip.
                tally: FoldedTally::of(&self.workspaces[ws_idx].threads, false),
                ui,
                // **Air, not a hairline**. The rail already
                // spends background on two things - the active project and
                // the selected surface - and a rule here would be a third
                // graphic language in a 250px column, competing with the
                // two that carry meaning. `divider` in this app means the
                // boundary of a *region* (rail from panes, header from
                // body); inside a region it would be a second meaning for
                // one mark.
                //
                // The reason that settles it is **collapse**. A folded
                // project is a single row, and a line above a single row
                // reads as a separator between two rows rather than between
                // two groups - there is nothing inside the group for it to
                // be the top of. Air is not a mark, so 8px reads the same
                // over a one-line group and a five-line one.
                lead_gap,
            },
            cx,
        ));

        if !is_expanded {
            return list;
        }

        // The meta line the design puts under an expanded container:
        // branch and diffstat, or the plain-directory notice.
        list = list.child(self.container_meta_row(ws_idx, ui, cx));

        // The design fixes the order inside a container: agent surfaces first,
        // then views, then shells. The list was in insertion order before,
        // and that - not what the list contains - is what read as a
        // jumble: a rail that mixes kinds has to be sorted by kind, or
        // every row looks like it landed where it did by accident.
        //
        // Indices stay tied to the underlying Vec position however the
        // rows are grouped, so `select_thread` / `remove_thread` still
        // resolve to the correct row.
        let SurfaceRowOrder {
            agents: agent_order,
            shells: shell_order,
        } = surface_row_order(&self.workspaces[ws_idx].threads);
        let mut rows_in_project = agent_order.len() + shell_order.len();
        for thread_idx in agent_order {
            let thread = &self.workspaces[ws_idx].threads[thread_idx];
            let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
            list = list.child(self.agents_thread_row_for(target, thread, shared, cx));
        }

        // Views: the container's diff, when it is open.
        for row in self.container_view_rows(ws_idx, shared, cx) {
            list = list.child(row);
            rows_in_project += 1;
        }

        // Shells: the parked ones, then every terminal in the slot tree.
        for thread_idx in shell_order {
            let thread = &self.workspaces[ws_idx].threads[thread_idx];
            let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
            list = list.child(self.agents_thread_row_for(target, thread, shared, cx));
        }
        for row in self.container_pane_rows(ws_idx, shared, cx) {
            list = list.child(row);
            rows_in_project += 1;
        }

        if rows_in_project == 0 {
            list = list.child(empty_project_hint(ui));
        }

        // The two ways to start a session, at the foot of the container
        // they start it in.
        list = list.child(self.new_session_buttons(ws_idx, ui, cx));
        list
    }

    /// Whether the agent surface `thread_id` is the *active* tab of the pane
    /// holding it. A background tab is loaded and one click from being shown,
    /// which is exactly the difference the rail is asked to mark.
    fn agent_is_shown(&self, ws_idx: usize, thread_id: u64, cx: &gpui::App) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
            .map(|root| {
                root.collect_leaves().into_iter().any(|pane| {
                    pane.read(cx)
                        .active_terminal_opt()
                        .and_then(|view| view.read(cx).agent_thread_id)
                        == Some(thread_id)
                })
            })
            .unwrap_or(false)
    }

    /// Build one surface row from a unified target + shared per-render state.
    /// Centralises the rename-input and selection wiring so every surface row
    /// in the rail is built by one path.
    fn agents_thread_row_for(
        &self,
        target: crate::project::AgentsTarget,
        thread: &crate::project::Thread,
        shared: &RowSharedState,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_shell = is_shell_surface(thread);
        let crate::project::AgentsTarget::Thread { ws_idx, .. } = target else {
            unreachable!("a surface row is always built from a Thread target")
        };
        // "Open" is the design's word for a session showing in a pane, and
        // that is now the whole of it: there is nowhere else a surface can be.
        // "Focused" is the finer state: open *and* taking input.
        let in_a_pane = shared.panes_on_screen == Some(ws_idx)
            && self.agent_surface_in_slot(ws_idx, thread.id, cx);
        let behind = in_a_pane && !self.agent_is_shown(ws_idx, thread.id, cx);
        let is_open = in_a_pane && !behind;
        let is_focused = shared.focus
            == Some(crate::app::workspace_ops::FocusedSurface::Agent {
                ws_idx,
                thread_id: thread.id,
            });
        self.thread_row(
            ThreadRowArgs {
                target,
                thread_id: thread.id,
                title: thread.title.clone(),
                rename_input: if is_renaming_target(shared.renaming, target) {
                    shared.rename_input.clone()
                } else {
                    None
                },
                is_open,
                is_focused,
                behind,
                is_pinned: thread.pinned,
                ui: shared.ui,
                status: thread.status,
                is_shell,
                unseen: thread.finished_unseen.is_some(),
                // Bound to the transition, never to this row being drawn: the
                // rail remounts its rows on every reorder, and an animation
                // keyed on mounting would replay the whole queue's worth of
                // pulses each time the order changed.
                announcing: self.thread_announcement(thread.id),
                // The design spends the row's right side on what the session
                // *is* rather than on how old it is: the agent's name, or the
                // word "shell".
                meta: if is_shell {
                    SharedString::from("shell")
                } else {
                    SharedString::from(
                        thread
                            .terminal_agent
                            .map(|agent| agent.display_name().to_string())
                            .unwrap_or_else(|| "agent".to_string()),
                    )
                },
            },
            cx,
        )
    }

    /// How many surfaces a container's rail row counts in its badge: its
    /// sessions, plus the shells living in the slot tree, plus `Changes` when
    /// it is open. The badge says how much is behind the caret, so it counts
    /// exactly the rows the caret reveals.
    fn container_surface_count(&self, ws_idx: usize, cx: &gpui::App) -> usize {
        let Some(container) = self.workspaces.get(ws_idx) else {
            return 0;
        };
        container.threads.len()
            + usize::from(container.diff_surface.is_some())
            + self.container_pane_row_count(ws_idx, cx)
    }

    /// Items rendered inside the bottom Settings popover - the one entry to
    /// the application layer, which has no scope and sits apart from the
    /// container tree. Order: the account-scope pages first,
    /// then the help links, then About and Settings.
    ///
    /// The help links came from a "Help" menu in the title bar. The title bar
    /// carries identity and global status, not content: what those links open
    /// has no scope inside the tree, and what has no scope belongs to the
    /// application layer - here, beside Settings and About. Its neighbour
    /// "Files" held only "New Workspace" and "Settings" - the rail's own `+`
    /// and this very gear - and is simply gone.
    fn rail_menu_items(&self) -> Vec<crate::app::sidebar_actions_menu::SidebarMenuItem> {
        use crate::app::sidebar_actions_menu::{SidebarMenuItem, help_url};
        vec![
            SidebarMenuItem {
                id: "agents-menu-skills".into(),
                icon: "icons/tool.svg",
                label: "Skills".into(),
                on_click: Box::new(|app, window, cx| {
                    app.show_agents_skills(window, cx);
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-themes".into(),
                icon: "icons/palette.svg",
                label: "Themes".into(),
                on_click: Box::new(|app, w, cx| {
                    app.open_theme_picker(w, cx);
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-documentation".into(),
                icon: "icons/file-text.svg",
                label: "Documentation".into(),
                on_click: Box::new(|app, _w, cx| {
                    app.open_help_url(help_url::DOCUMENTATION, cx);
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-whats-new".into(),
                icon: "icons/rocket.svg",
                label: "What's New".into(),
                on_click: Box::new(|app, _w, cx| {
                    app.open_help_url(help_url::RELEASES, cx);
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-automations".into(),
                icon: "icons/bolt.svg",
                label: "Automations".into(),
                on_click: Box::new(|app, _w, cx| {
                    app.open_help_url(help_url::AUTOMATIONS, cx);
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-review-docs".into(),
                icon: "icons/git-pull-request.svg",
                label: "Reviewing changes".into(),
                on_click: Box::new(|app, _w, cx| {
                    app.open_help_url(help_url::REVIEW, cx);
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-troubleshooting".into(),
                icon: "icons/bug.svg",
                label: "Troubleshooting".into(),
                on_click: Box::new(|app, _w, cx| {
                    app.open_help_url(help_url::TROUBLESHOOTING, cx);
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-about".into(),
                icon: "icons/info-circle.svg",
                label: "About Splitlane".into(),
                on_click: Box::new(|app, _w, cx| {
                    app.show_about_dialog = true;
                    cx.notify();
                }),
            },
            SidebarMenuItem {
                id: "agents-menu-open-settings".into(),
                icon: "icons/settings.svg",
                label: "Settings".into(),
                on_click: Box::new(|app, w, cx| {
                    app.open_settings_window(w, cx);
                }),
            },
        ]
    }

    /// The container's view surfaces, as rail rows: its git diff.
    fn container_view_rows(
        &self,
        ws_idx: usize,
        shared: &RowSharedState,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        use crate::project::AgentsTarget;
        let Some(container) = self.workspaces.get(ws_idx) else {
            return Vec::new();
        };
        let ui = shared.ui;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();

        // Only when it has been opened. A row for every container whether or
        // not anyone asked for it is a menu entry, not a surface.
        if container.diff_surface.is_some() {
            let target = AgentsTarget::Diff { ws_idx };
            let is_focused =
                shared.focus == Some(crate::app::workspace_ops::FocusedSurface::Diff { ws_idx });
            rows.push(self.container_surface_row(
                SurfaceRowArgs {
                    id: SharedString::from(format!("rail-changes-{}", container.id)),
                    glyph: SharedString::from("◈"),
                    // The third type glyph, on the same neutral step as the
                    // other two. It was `text_tertiary`, which is
                    // one step off `dim` and said nothing by being different.
                    glyph_color: ui.dim,
                    label: SharedString::from("Changes"),
                    meta: SharedString::from("diff"),
                    target,
                    // The diff lives in a pane like everything else, so
                    // "open" is the same question it is for a shell: is one of
                    // this container's panes showing it, and is that container
                    // the one on screen.
                    is_open: shared.panes_on_screen == Some(ws_idx)
                        && pane_showing_diff(container, cx).is_some(),
                    is_focused,
                    behind: shared.panes_on_screen == Some(ws_idx)
                        && pane_holding_diff(container, cx).is_some()
                        && pane_showing_diff(container, cx).is_none(),
                    // Clicking the row hands input to the pane the diff is in,
                    // raising its tab when something else is over it.
                    focus_pane: pane_holding_diff(container, cx),
                    focus_tab: pane_holding_diff(container, cx).and_then(|pane| {
                        pane.read(cx)
                            .tabs
                            .iter()
                            .position(|tab| matches!(tab, crate::pane::TabContent::Diff(_)))
                    }),
                    // A surface that opens has to close, or "opened" is just a
                    // slower kind of permanent. Nothing is lost by closing it -
                    // the diff is recomputed from git on the next open - so the
                    // `×` acts immediately and asks nothing.
                    trailing: Some(close_surface_button(
                        SharedString::from(format!("rail-changes-close-{}", container.id)),
                        ws_idx,
                        !rail_overlay_open(self),
                        ui,
                        cx,
                    )),
                    ui,
                },
                cx,
            ));
        }
        rows
    }

    /// Slot-tree leaves that are plain shells - the rows [`Self::container_pane_rows`]
    /// draws, counted without building them.
    fn container_pane_row_count(&self, ws_idx: usize, cx: &gpui::App) -> usize {
        self.workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
            .map(|root| {
                root.collect_leaves()
                    .into_iter()
                    .filter(|pane| {
                        // The same three exclusions the rows themselves make,
                        // or the caret's badge counts a row it will not draw.
                        !(pane_shows_agent_surface(pane, cx)
                            || pane_shows_diff(pane, cx)
                            || pane_is_empty(pane, cx))
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// One row per terminal in the container's slot tree.
    fn container_pane_rows(
        &self,
        ws_idx: usize,
        shared: &RowSharedState,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        use crate::project::AgentsTarget;
        let Some(container) = self.workspaces.get(ws_idx) else {
            return Vec::new();
        };
        let ui = shared.ui;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let panes_target = AgentsTarget::Panes { ws_idx };
        for (leaf_idx, pane) in container
            .root
            .as_ref()
            .map(|root| root.collect_leaves())
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let on_screen = shared.panes_on_screen == Some(ws_idx);
            let pane_focused = shared.focus
                == Some(crate::app::workspace_ops::FocusedSurface::Slot { ws_idx, leaf_idx });
            // A pane with no session in it is not in this list. The rail is
            // the inventory of what exists, and an empty pane names no
            // surface and holds no work - "an invitation, not a state worth
            // keeping", which is also why restore drops it. It used to draw a
            // row reading `New session \u{b7} launcher` with nothing on it,
            // and it read as an object that was not one.
            if pane_is_empty(&pane, cx) {
                continue;
            }
            // One row per SURFACE, not per pane. A pane holding two
            // things used to draw one row named after whichever was on top, so
            // raising a tab took the other surface out of the rail entirely -
            // and the rail is the inventory. The tab strip says what the pane
            // holds; this list says what exists and which of it is visible.
            let active_idx = pane.read(cx).selected_idx;
            let tabs = pane.read(cx).tabs.clone();
            for (tab_idx, tab) in tabs.iter().enumerate() {
                // An agent surface is already in this list, under its own row
                // with its status and its menu; naming it again as a shell
                // would put one surface in the rail twice.
                if tab
                    .as_terminal()
                    .is_some_and(|view| view.read(cx).agent_thread_id.is_some())
                {
                    continue;
                }
                // Same for the diff: that is the `Changes` row above, and it
                // carries the close `\u{d7}` this one does not.
                if matches!(tab, crate::pane::TabContent::Diff(_)) {
                    continue;
                }
                let shown = tab_idx == active_idx;
                let (glyph, meta) = match tab {
                    crate::pane::TabContent::Markdown(file) => {
                        let file = file.read(cx);
                        (file.glyph(), file.row_word())
                    }
                    _ => ("\u{25b7}", "shell"),
                };
                let label = crate::pane::Pane::tab_title(tab, cx);
                rows.push(self.container_surface_row(
                    SurfaceRowArgs {
                        id: SharedString::from(format!(
                            "rail-pane-{}-{leaf_idx}-{tab_idx}",
                            container.id
                        )),
                        glyph: SharedString::from(glyph),
                        glyph_color: ui.dim,
                        label: SharedString::from(label),
                        meta: SharedString::from(meta),
                        target: panes_target,
                        // A slot is on screen only while its container's tree
                        // is what the content area shows. Every container keeps
                        // its panes alive in the background, and saying "open"
                        // for those lit the rows of containers the user cannot
                        // see.
                        is_open: on_screen && shown,
                        // One slot of the tree, and the tab it is showing:
                        // input reaches the active tab of the focused pane and
                        // nothing else.
                        is_focused: pane_focused && shown,
                        behind: on_screen && !shown,
                        // Clicking the row raises this tab in its pane and
                        // hands it the keyboard.
                        focus_pane: Some(pane.clone()),
                        focus_tab: Some(tab_idx),
                        trailing: None,
                        ui,
                    },
                    cx,
                ));
            }
        }
        rows
    }

    /// One surface row under a container header. Same geometry as an agent
    /// surface's row so the three kinds read as one list.
    fn container_surface_row(
        &self,
        args: SurfaceRowArgs,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let SurfaceRowArgs {
            id,
            glyph,
            glyph_color,
            label,
            meta,
            target,
            is_open,
            is_focused,
            behind,
            focus_pane,
            focus_tab,
            trailing,
            ui,
        } = args;
        let mut row = surface_row_frame(id, is_open, is_focused, ui)
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.close_agents_menu(cx);
                this.commit_agents_rename(cx);
                this.select_agents_target(target, cx);
                // After the selection, which aims focus at the container's
                // first slot: this row knows a better answer. Focus itself is
                // deferred to the next render, where the `Window` is.
                if let Some(pane) = focus_pane.clone() {
                    if let Some(tab_idx) = focus_tab {
                        pane.update(cx, |pane, cx| {
                            if tab_idx < pane.tabs.len() {
                                pane.selected_idx = tab_idx;
                            }
                            cx.notify();
                        });
                    }
                    this.pending_pane_focus = Some(pane);
                }
            }))
            .on_aux_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                if e.is_right_click()
                    && let Some(position) = e.mouse_position()
                {
                    this.commit_agents_rename(cx);
                    this.open_agents_menu_for_target(target, position, MenuOrigin::RailRow, cx);
                    cx.stop_propagation();
                }
            }))
            .child(surface_row_glyph(glyph, glyph_color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(if is_open {
                        ui.text
                    } else if behind {
                        ui.muted
                    } else {
                        ui.text_tertiary
                    })
                    .truncate()
                    .child(label),
            )
            .child(surface_row_meta(meta, ui));
        row = match trailing {
            Some(trailing) => row.child(trailing),
            None => row.child(status_dot("rail-pane-row", None, false, None, ui)),
        };
        row.into_any_element()
    }

    /// The line the design puts under an expanded container: its branch and
    /// its diffstat, or - for a plain directory - the one sentence that says
    /// why neither is there.
    fn container_meta_row(
        &self,
        ws_idx: usize,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(container) = self.workspaces.get(ws_idx) else {
            return div().into_any_element();
        };
        let has_repo = container.repo_root.is_some();
        let branch = container.git_branch.clone();
        let stats = container.git_stats.clone();
        let diff = ui.diff_colors();

        let line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .font_family(tok::font::MONO)
            .text_size(tok::mono::LABEL)
            .pl(RAIL_SURFACE_MARGIN_L + RAIL_SURFACE_INDENT)
            .pr(RAIL_ROW_PADDING_X)
            .pt(tok::space::XS)
            .pb(tok::space::SM);

        if !has_repo {
            // A container can be a plain directory. Saying so beats an empty
            // line that reads as "the branch has not loaded yet".
            return line
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(ui.faint)
                        .child("not a git repository"),
                )
                .into_any_element();
        }

        let has_stats = !stats.is_empty();
        line.child(
            div()
                // `min_w_0` + `truncate` needs a basis to shrink *from*:
                // without `flex_1` the branch got zero width and rendered as
                // its ellipsis alone, so the row read as three dots taking up
                // space for no reason - which is exactly how it was reported.
                // The diffstat beside it is `flex_none`, so this takes the
                // rest and ellipsizes only a branch that genuinely does not
                // fit.
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(ui.muted)
                .child(if branch.is_empty() {
                    SharedString::from("detached")
                } else {
                    SharedString::from(branch)
                }),
        )
        .when(has_stats, |line| {
            // The counters are also the way in: the design opens the diff from
            // them and from the file tree's Review button, and the tree is not
            // built yet - so this is the mouse path to `Changes`.
            line.child(
                div()
                    .id(SharedString::from(format!(
                        "rail-diffstat-{}",
                        container.id
                    )))
                    .role(Role::Button)
                    .aria_label("Open changes")
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::XS)
                    .rounded(tok::radius::BADGE)
                    .when(!rail_overlay_open(self), |chip| {
                        chip.tooltip(crate::ui_primitives::text_tooltip("Open changes"))
                    })
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.close_agents_menu(cx);
                        this.commit_agents_rename(cx);
                        this.open_container_diff_surface(ws_idx, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .text_color(diff.added)
                            .child(format!("+{}", stats.insertions)),
                    )
                    .child(
                        div()
                            .text_color(diff.deleted)
                            .child(format!("-{}", stats.deletions)),
                    ),
            )
        })
        .into_any_element()
    }

    fn project_header_row(
        &self,
        args: ProjectHeaderArgs,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ProjectHeaderArgs {
            ws_idx,
            project_id,
            title,
            is_expanded,
            rename_input,
            surface_count,
            tally,
            ui,
            lead_gap,
        } = args;
        // The container header follows the container owning the focused pane,
        // which is this: only the active container's tree is rendered, and every path
        // that puts focus in a pane of container X makes X active first
        // (`activate_workspace_at`, `select_container_surface`,
        // `reveal_agent_in_slot`). Asking `focused_surface` instead would say
        // the same thing while the panes are up and say *nothing* over the
        // launcher, the Skills page and Settings - where the design still
        // wants the active project marked.
        let is_active = self.active_idx == ws_idx;
        let drag_title: SharedString = title.clone().into();
        let drag_branch = self
            .workspaces
            .get(ws_idx)
            .map(|ws| ws.git_branch.clone())
            .filter(|branch| !branch.is_empty())
            .map(SharedString::from);
        let cwd_tooltip: SharedString = self
            .workspaces
            .get(ws_idx)
            .map(|ws| crate::app::sidebar::collapse_home(&ws.cwd, &self.home_dir))
            .unwrap_or_default()
            .into();
        // The design marks the active project with the same "selected row"
        // token the surface rows use, so the container and what is open inside
        // it read as one selection at two levels.
        let hover_background = ui.subtle;
        let resting_background = if is_active {
            hover_background
        } else {
            hover_background.opacity(0.0)
        };

        // Title element switches between read-only and inline-input
        // mode. The input is a full [`TextArea`] entity (cursor,
        // selection, IME, clipboard, click-to-position, double-click
        // word select) -- same widget the chat composer uses, so the
        // editing experience is consistent across the app.
        let title_el: gpui::AnyElement = if let Some(input) = rename_input {
            div()
                .flex_1()
                .min_w_0()
                .bg(ui.overlay)
                .px_1()
                .rounded(tok::radius::METER)
                .child(input)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .text_color(if is_active {
                    ui.text
                } else {
                    ui.text_secondary
                })
                .truncate()
                .child(title)
                .into_any_element()
        };

        let row = div()
            .id(SharedString::from(format!("agents-project-{project_id}")))
            .px(RAIL_ROW_PADDING_X)
            // On the margin rather than inside the row: the row's fill marks
            // the active project, and 8px of it above would read as the
            // selection reaching up into the gap.
            .when(lead_gap, |row| row.mt(tok::space::MD))
            .h(tok::row::PROJECT)
            .rounded(tok::radius::SMALL)
            .flex()
            .flex_row()
            .items_center()
            .gap(RAIL_ROW_GAP)
            // A container's name is code - it is a directory - and the design
            // sets the whole rail in mono for that reason.
            .font_family(tok::font::MONO)
            .font_weight(FontWeight::MEDIUM)
            .text_size(tok::mono::BODY)
            .bg(resting_background)
            .on_drag(
                crate::app::drag::WorkspaceDrag {
                    id: project_id,
                    source_idx: ws_idx,
                    title: drag_title.clone(),
                    branch: drag_branch.clone(),
                },
                |drag, _offset, _window, cx| {
                    cx.new(|_| crate::WorkspaceDragPreview {
                        title: drag.title.clone(),
                        branch: drag.branch.clone(),
                    })
                },
            )
            .cursor_pointer()
            .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| {
                this.close_agents_menu(cx);
                let is_double = matches!(e, ClickEvent::Mouse(m) if m.down.click_count == 2);
                if is_double {
                    // Double-click -> inline rename mode.
                    this.begin_agents_rename(AgentsRenameTarget::Project { ws_idx }, w, cx);
                } else {
                    // Single click makes the container active and opens it. It
                    // does not close it, and that asymmetry is the whole point:
                    // the row used to *toggle*, so the only way to fold a
                    // project away was to click its row - which also switched
                    // the content area to it, taking the panes off whatever the
                    // user was actually looking at. Folding a project one is not
                    // in is the commonest reason to touch that row, and it was
                    // the one gesture that could not be made without a side
                    // effect on the rest of the screen.
                    //
                    // So the two questions are two affordances now: the caret
                    // folds (below, and only folds), the row selects. Every file
                    // tree worth copying is arranged this way.
                    this.commit_agents_rename(cx);
                    this.active_idx = ws_idx;
                    if let Some(project) = this.workspaces.get_mut(ws_idx) {
                        project.is_expanded = true;
                    }
                    this.save_session(cx);
                    cx.notify();
                }
            }))
            .on_aux_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                if e.is_right_click()
                    && let Some(position) = e.mouse_position()
                {
                    this.commit_agents_rename(cx);
                    this.open_agents_project_menu(ws_idx, position, cx);
                    cx.stop_propagation();
                }
            }))
            .child(disclosure_caret(ws_idx, project_id, is_expanded, ui, cx))
            // The path, but not while anything is open over the rail. GPUI
            // owns tooltips and the app owns menus, so nothing else settles
            // the z-order: a tooltip drawn over a menu covers the entry under
            // the pointer, which is the one the menu was opened to pick.
            //
            // Asked through `rail_overlay_open`, because this guard has been
            // written three times - once per overlay someone noticed - and
            // each time it knew about a different subset.
            .when(!rail_overlay_open(self), |row| {
                row.tooltip(move |_w, cx| {
                    cx.new(|_| crate::app::sidebar::WorkspaceCwdTooltip {
                        path: cwd_tooltip.clone(),
                    })
                    .into()
                })
            })
            .child(title_el)
            // The folded group answering for the rows it is not drawing: the
            // news its rows would have carried, and the work going
            // on inside it. Before the count badge rather than
            // inside it: the badge answers "how much is behind this caret",
            // which is a fact about the project, and these answer what is
            // happening in it and what the reader has not seen.
            .children(
                (!is_expanded)
                    .then(|| folded_project_tally(tally.failed, tally.running, tally.finished, ui))
                    .flatten(),
            )
            // The design's container row carries one trailing number and no
            // status, and it can now: "N agents waiting" lives in the title
            // bar's chip (or, under a system frame, at the right end of the
            // toolbar), which is on screen whatever this caret is doing. P3.1
            // drew the aggregate here because without the chip a collapsed
            // container was the one place an agent waiting could be invisible.
            // How much is behind the caret. The design gives the container row
            // exactly one trailing number, and it is this one.
            ;

        // The count gives way to the row's own actions under the pointer and
        // comes back when it leaves - one slot, two readings, no shift. Same
        // arrangement as a session row, and for the same reason: a container
        // is an object with actions of its own, and until now the only way to
        // reach them was a right-click into a menu that scrolled without
        // saying so, which is how "there is no way to delete a project"
        // became true in practice.
        let count_slot = div()
            .flex_none()
            .px(tok::space::XS)
            // Physical, not rhythmic: one pixel of optical air around a
            // 10px numeral inside a 28px row.
            .py(px(1.))
            .rounded(tok::radius::BADGE)
            .bg(ui.subtle)
            .text_size(tok::mono::LABEL)
            .font_weight(FontWeight::NORMAL)
            .text_color(ui.dim)
            .child(surface_count.to_string())
            .into_any_element();
        let container_target = crate::project::AgentsTarget::Panes { ws_idx };
        let tooltips_ok = !rail_overlay_open(self);
        let container_actions =
            hover_actions_cluster(container_target, project_id, None, tooltips_ok, ui, cx);
        // One animator, not two: the row's fill and its trailing slot move
        // together, and `animated_hover_bg` would have claimed the only slot.
        let row = row.animated_hover_element(move |row, delta| {
            row.style()
                .bg(lerp_color(resting_background, hover_background, delta));
            let count_opacity = 1.0 - delta;
            let count = div()
                .flex_none()
                .flex()
                .justify_end()
                .min_w(hover_actions_reserve(None) * delta)
                .opacity(count_opacity)
                .child(count_slot)
                .into_any_element();
            let actions_opacity = delta;
            let actions = container_actions
                .opacity(actions_opacity)
                .when(actions_opacity <= f32::EPSILON, |slot| slot.hidden())
                .into_any_element();
            row.extend([count, actions]);
        });

        // Drag-to-reorder, carried over from the CLI rail: the container list
        // is the one list now, so its order is the only order there is.
        let ws_id = project_id;
        div()
            .id(SharedString::from(format!("rail-drop-{project_id}")))
            .flex_none()
            .flex()
            .flex_col()
            .rounded(tok::radius::SMALL)
            .drag_over::<crate::app::drag::WorkspaceDrag>(move |style, drag, _window, _cx| {
                let indicator = ui.text.opacity(0.4);
                let target_background =
                    crate::app::constants::sidebar_tab_active_background().opacity(0.24);
                match crate::app::sidebar::workspace_drop_edge(drag, ws_id, ws_idx) {
                    Some(crate::app::sidebar::WorkspaceDropEdge::Before) => style
                        .border_t_1()
                        .border_color(indicator)
                        .bg(target_background),
                    Some(crate::app::sidebar::WorkspaceDropEdge::After) => style
                        .border_b_1()
                        .border_color(indicator)
                        .bg(target_background),
                    None => style,
                }
            })
            .on_drop(cx.listener(
                move |this, drag: &crate::app::drag::WorkspaceDrag, _window, cx| {
                    this.reorder_workspace(drag.id, ws_idx, cx);
                },
            ))
            .child(row)
            .into_any_element()
    }

    fn thread_row(&self, args: ThreadRowArgs, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ThreadRowArgs {
            behind,
            target,
            thread_id,
            title,
            rename_input,
            is_open,
            is_focused,
            is_pinned,
            ui,
            status,
            is_shell,
            unseen,
            announcing,
            meta,
        } = args;
        let is_renaming = rename_input.is_some();
        // Snapshotted before the title is moved into the row, for the drag
        // ghost.
        let drag_title = SharedString::from(title.clone());
        // Inline delete-confirm: this row is "armed" (shows a red Delete button
        // in place of the trash icon) when its target matches the armed slot.

        let title_el: gpui::AnyElement = if let Some(input) = rename_input {
            // Inline rename input -- full TextArea entity (same
            // widget the composer uses) so the user gets real cursor,
            // selection, IME, copy/paste, click-to-position, double-
            // click word select.
            div()
                .flex_1()
                .min_w_0()
                .bg(ui.overlay)
                .px_1()
                .rounded(tok::radius::METER)
                .child(input)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                // Three weights: shown, loaded-but-behind, listed -
                // and a fourth reading, unseen, on top of them. An unseen row
                // keeps the top of the ramp the way an unread mail row does,
                // which is the whole of the mark's weight: no accent, no new
                // colour, nothing borrowed from the status vocabulary.
                .text_color(if is_open || unseen {
                    ui.text
                } else if behind {
                    ui.muted
                } else {
                    ui.text_tertiary
                })
                .truncate()
                .child(title)
                .into_any_element()
        };

        let tooltips_ok = !rail_overlay_open(self);
        let hover_actions =
            hover_actions_cluster(target, thread_id, Some(is_pinned), tooltips_ok, ui, cx);
        let meta_slot = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(RAIL_ROW_GAP)
            // The news takes the meta slot rather than sitting beside it: the
            // row is 25px and three columns already, and what the session *is*
            // comes back the moment the news has been read.
            //
            // **The slot names how the run ended, not that it ended**. The slot
            // was first given to the news without having to say
            // what the news *is*, because there was one terminal word then;
            // `finished` was never a claim that a run reached its terminus, it
            // was the name of the outcome the reader has not had yet - and when
            // that outcome is a failure, its name is `failed`. That makes the
            // chip and this row agree by construction rather than by
            // coincidence, and any terminal word the scale grows later lands
            // here with no further decision.
            //
            // **Not nothing**, which was the other candidate: the red dot would
            // then carry the whole message alone, and that door is shut -
            // every coloured signal in this interface has a word beside it and
            // every word has a colour. A failed run whose only evidence is a
            // red 6px dot is the case that rule exists to prevent.
            .child(if unseen {
                surface_row_news(news_word(status), ui)
            } else {
                surface_row_meta(meta, ui)
            })
            .child(status_dot(
                "rail-surface-row",
                Some(status),
                unseen,
                announcing,
                ui,
            ))
            .into_any_element();

        let mut row = surface_row_frame(
            SharedString::from(format!("agents-project-thread-{thread_id}")),
            is_open,
            is_focused,
            ui,
        )
        .relative();

        // A parked surface can be dragged into the panes area: onto the dashed
        // strip for a third pane, or onto a pane to show it there. One already
        // in a slot is not draggable - it has no third place to go, and the
        // design draws the strip only for "a session that is not already open".
        if !is_open && let crate::project::AgentsTarget::Thread { ws_idx, .. } = target {
            let icon = SharedString::from(if is_shell {
                "icons/terminal.svg"
            } else {
                "icons/sparkles.svg"
            });
            let app = cx.weak_entity();
            row = row.on_drag(
                crate::pane_drag::RailSurfaceDrag {
                    ws_idx,
                    thread_id,
                    title: drag_title,
                    icon,
                },
                move |drag, _offset, _window, cx| {
                    // GPUI hands out no way to ask "is a drag of THIS type in
                    // flight", and the strip has to know. So the app records
                    // it here and clears it in `render` the moment GPUI
                    // reports no drag at all.
                    let _ = app.update(cx, |app, _| {
                        app.dragging_rail_surface = Some(drag.clone());
                    });
                    cx.new(|_| crate::pane_drag::TabDragPreview {
                        title: drag.title.clone(),
                        icon: drag.icon.clone(),
                    })
                },
            );
        }

        row.animated_hover_element(move |row, delta| {
            // The meta and the dot give way to the row's own actions, and come
            // back when the pointer leaves - one slot, two readings, no shift.
            let meta_opacity = 1.0 - delta;
            let meta_slot = div()
                .flex_none()
                .flex()
                .justify_end()
                .min_w(hover_actions_reserve(Some(is_pinned)) * delta)
                .opacity(meta_opacity)
                .child(meta_slot)
                .into_any_element();

            let actions_opacity = delta;
            let hover_actions = hover_actions
                .opacity(actions_opacity)
                .when(actions_opacity <= f32::EPSILON, |actions| actions.hidden())
                .into_any_element();
            row.extend([meta_slot, hover_actions]);
        })
        .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| {
            this.close_agents_menu(cx);
            // While the row is in rename mode we let the click
            // pass through to the embedded TextArea (which then
            // positions the caret / extends the selection). The
            // row's own selection is skipped so an in-place mouse
            // click on the input doesn't navigate away.
            if is_renaming {
                return;
            }
            // Double-click renames in place, the way a container row above it
            // already did. One gesture over a name in this rail, whatever the
            // row names - and the same gesture the pane header answers to, so
            // a surface is renamed the same way in both places it is shown.
            // Rename lived only in this row's right-click menu, which is a
            // different gesture for the same act one level down.
            if matches!(e, ClickEvent::Mouse(m) if m.down.click_count == 2) {
                this.begin_agents_rename_for_target(target, w, cx);
                return;
            }
            this.commit_agents_rename(cx);
            this.select_agents_target(target, cx);
        }))
        .on_aux_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
            if e.is_right_click()
                && let Some(position) = e.mouse_position()
            {
                this.commit_agents_rename(cx);
                this.open_agents_menu_for_target(target, position, MenuOrigin::RailRow, cx);
                cx.stop_propagation();
            }
        }))
        .child(surface_type_glyph(is_shell, ui))
        .child(title_el)
        .into_any_element()
    }
}

/// Argument carrier for [`SplitlaneApp::project_header_row`]. Lets the
/// fn signature stay under clippy's `too_many_arguments` threshold
/// while keeping the per-row state explicit at the call site.
struct ProjectHeaderArgs {
    ws_idx: usize,
    project_id: u64,
    title: String,
    is_expanded: bool,
    /// `Some` when this row is the current inline-rename target; the
    /// entity is rendered in place of the static title and owns its
    /// own keyboard / mouse handling (cursor, selection, IME, ...).
    rename_input: Option<gpui::Entity<crate::widgets::text_area::TextArea>>,
    /// What the row says for its sessions while the caret is **closed**.
    ///
    /// Only read while folded, which is the half of the problem a window-wide
    /// counter could never solve: a folded project draws no session rows at
    /// all, so the marks on them have nowhere to appear. Expanded, the rows
    /// say it themselves and a tally beside them would say it twice.
    ///
    /// `finished` is **news** and clears when the reader looks; `failed` and
    /// `running` are **state** and do not. No session is counted twice.
    tally: FoldedTally,
    /// How many rows the caret reveals - the badge's number.
    surface_count: usize,
    ui: crate::theme::UiColors,
    /// Whether this row opens a group that is not the first, and so takes the
    /// air that separates one project from the one above it.
    lead_gap: bool,
}

/// Argument carrier for a surface row that is not an agent surface: the
/// container's `Changes`, and each shell in its slot tree.
struct SurfaceRowArgs {
    id: SharedString,
    /// The type glyph, in the design's own vocabulary: `◆` agent, `▷` shell,
    /// `◈` diff.
    glyph: SharedString,
    glyph_color: gpui::Hsla,
    label: SharedString,
    /// The one word that says what kind of surface this is.
    meta: SharedString,
    target: crate::project::AgentsTarget,
    /// Showing in a slot (or, for a parked surface, showing full-area).
    is_open: bool,
    /// Showing *and* taking input.
    is_focused: bool,
    /// Loaded, but behind its pane's active tab. A surface never
    /// leaves the rail because something was raised over it - the tab strip
    /// says what a pane *holds*, the rail says what exists and which of it is
    /// visible. The row stays and dims.
    behind: bool,
    /// The slot this row stands for, when it stands for one: clicking the row
    /// hands input to it, on top of selecting `target`.
    focus_pane: Option<gpui::Entity<crate::Pane>>,
    /// Which tab of that slot, when the row stands for one surface of several.
    /// Clicking raises it first: a row that says "open, behind something" has
    /// to be one click from being shown, or the mark is a description of a
    /// dead end.
    focus_tab: Option<usize>,
    trailing: Option<gpui::AnyElement>,
    ui: crate::theme::UiColors,
}

struct ThreadRowArgs {
    /// Loaded, but behind its pane's active tab (see `SurfaceRowArgs`).
    behind: bool,
    /// The unified selection target this row drives.
    target: crate::project::AgentsTarget,
    thread_id: u64,
    title: String,
    /// `Some` when this row is the current inline-rename target; the
    /// entity is rendered in place of the static title and owns its
    /// own keyboard / mouse handling (cursor, selection, IME, ...).
    rename_input: Option<gpui::Entity<crate::widgets::text_area::TextArea>>,
    /// Showing in a slot, or showing full-area as the current selection.
    is_open: bool,
    /// Showing *and* holding the selection - the row that gets the accent bar.
    is_focused: bool,
    /// Whether this surface is pinned (drives the hover ★/☆ toggle
    /// glyph, and lifts the row to the top of its container).
    is_pinned: bool,
    ui: crate::theme::UiColors,
    /// Live agent-turn state (driven by the `ai.*` IPC hooks), read by the
    /// row's status dot.
    status: crate::project::ThreadStatus,
    /// A bare shell rather than a launched agent.
    is_shell: bool,
    /// What the surface is, in one word: the agent's name, or "shell".
    meta: SharedString,
    /// Whether this surface's run ended while nobody could see it.
    ///
    /// Not a status - the dot already says `idle`, honestly. This is the
    /// read/unread axis: the name keeps full brightness the way an unread row
    /// does, and the meta slot says `finished` until the row is opened.
    unseen: bool,
    /// The announcement when this session is one that made the queue's edge,
    /// `None` otherwise.
    announcing: Option<crate::app::announce::AnnouncePhase>,
}

/// Per-render state shared by every surface row, captured once in
/// [`SplitlaneApp::render_rail`] so the rows build without re-reading `self`
/// per row.
struct RowSharedState {
    /// What takes input this frame. A row is *open* when one of the panes on
    /// screen is showing its surface; this decides which single one of those
    /// wears the accent bar.
    focus: Option<crate::app::workspace_ops::FocusedSurface>,
    /// The container whose slot tree the content area is showing, if it is
    /// showing one at all. Panes of every other container are alive but off
    /// screen, and an off-screen slot is not an open surface.
    panes_on_screen: Option<usize>,
    renaming: Option<AgentsRenameTarget>,
    rename_input: Option<gpui::Entity<crate::widgets::text_area::TextArea>>,
    ui: crate::theme::UiColors,
}

/// Does the active inline-rename target point at `target`'s row? Maps the
/// rename enum (which still discriminates project rows from chat rows) onto
/// the unified [`crate::project::AgentsTarget`].
fn is_renaming_target(
    renaming: Option<AgentsRenameTarget>,
    target: crate::project::AgentsTarget,
) -> bool {
    use crate::project::AgentsTarget;
    matches!(
        (renaming, target),
        (
            Some(AgentsRenameTarget::Thread {
                ws_idx: rp,
                thread_idx: rt,
            }),
            AgentsTarget::Thread {
                ws_idx,
                thread_idx,
            },
        ) if rp == ws_idx && rt == thread_idx
    )
}

/// The frame every surface row shares: the indent that puts it a level under
/// its container, the two states the design gives it, and the accent bar that
/// marks the focused one.
fn surface_row_frame(
    id: SharedString,
    is_open: bool,
    is_focused: bool,
    ui: crate::theme::UiColors,
) -> gpui::Stateful<gpui::Div> {
    // Three backgrounds, not two: nothing when the surface is only listed or
    // is loaded behind a tab, the neutral selected wash when it is the one its
    // pane is showing, and the accent wash - plus a 2px bar - for the one the
    // user is actually in.
    //
    // The middle one is `subtle`, which is the design's own "hover / selected
    // row" token. It used to be the CLI sidebar's selected-tab wash - white at
    // 11% over the rail - which is close to four times the step the design
    // draws, and made a merely-open surface shout as loud as the focused one.
    let background = if is_focused {
        ui.accent_surface
    } else if is_open {
        ui.subtle
    } else {
        gpui::transparent_black()
    };
    div()
        .id(id)
        .ml(RAIL_SURFACE_MARGIN_L)
        .pl(RAIL_SURFACE_INDENT)
        .pr(RAIL_ROW_PADDING_X)
        .h(tok::row::SESSION)
        .rounded(tok::radius::TAG)
        .flex()
        .flex_row()
        .items_center()
        .gap(RAIL_ROW_GAP)
        .font_family(tok::font::MONO)
        .text_size(tok::mono::INLINE)
        .bg(background)
        .cursor_pointer()
        .when(is_focused, |row| {
            // "inset 2px 0 0 accent": GPUI has no inset shadow, so the bar is
            // a real 2px child pinned to the row's left edge.
            row.relative().child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    // Physical: a rule's thickness, not a step of the scale.
                    .w(px(2.))
                    .bg(ui.accent),
            )
        })
}

/// The type glyph of a surface, in the design's own vocabulary: a filled
/// diamond for an agent, a hollow triangle for a shell. Type only - the row
/// says *state* on its status dot, and splitting the two is what lets a row
/// answer both questions at once. Shared with the slot header so one surface
/// reads the same in the rail and above the pane showing it.
///
/// # `dim`, and never the accent
///
/// The comment above was true and the code contradicted it: the agent glyph was
/// painted in `ui.accent`, which in the very same row - seven pixels to the
/// right, on the status dot - means **waiting for you**. One hue, two meanings,
/// and the left one lit permanently while the right one lights rarely and for a
/// reason. That is why it was found in use that one of the two marks looked
/// redundant: the always-on green diamond *read* as a state marker because it
/// was wearing a state colour.
///
/// The rule the designer drew out of it is worth more than the fix: **"one word,
/// one meaning" governs colour, not just wording.** The accent hue is one of
/// about six words this interface has, and spending it on decoration anywhere
/// makes it unavailable for meaning everywhere.
///
/// Both marks stand, and the hue difference between the two glyphs went too:
/// `◆` and `▷` both answer *which kind*, and a colour gap between them said
/// "one of these kinds matters more", which is not true.
pub(crate) fn surface_type_glyph(is_shell: bool, ui: crate::theme::UiColors) -> gpui::AnyElement {
    surface_row_glyph(
        SharedString::from(if is_shell { "\u{25b7}" } else { "\u{25c6}" }),
        ui.dim,
    )
}

/// A surface row's leading type glyph, in the fixed box that keeps every
/// label in the rail starting at the same x.
fn surface_row_glyph(glyph: SharedString, color: gpui::Hsla) -> gpui::AnyElement {
    div()
        .flex_none()
        .w(RAIL_CARET_WIDTH)
        .flex()
        .items_center()
        .text_size(tok::mono::HINT)
        .text_color(color)
        .child(glyph)
        .into_any_element()
}

/// The one word on a surface row that says what it is: the agent's name, or
/// "shell", or "diff".
fn surface_row_meta(meta: SharedString, ui: crate::theme::UiColors) -> gpui::AnyElement {
    div()
        .flex_none()
        .text_size(tok::mono::HINT)
        .text_color(ui.faint)
        .child(meta)
        .into_any_element()
}

/// The meta slot when the row is holding news: one plain word, a step brighter
/// than the meta it replaces.
///
/// `text_tertiary`, and the number is why. It was `dim`, on an earlier
/// "one step brighter" - but against the rail `dim` is 4.01 : 1 and
/// `faint` is 3.17, a **1.27 : 1** step, which is the pair already thrown out
/// for the dark idle pane border: one the eye cannot compare. So the tonal half
/// of the mark was doing nothing, and the whole signal rested on the name and
/// on the word simply being different. `text_tertiary` puts the step at
/// 2.19 : 1, the same size as the name's `text` over `text_tertiary` (2.15) -
/// two carriers of one magnitude.
///
/// The objection, and why it does not decide: `text_tertiary` is the colour of
/// an ordinary row's *name*, so a word this bright in the meta column could
/// pull the eye off the names. It sits in a different column, in mono at
/// `HINT`, and it is **self-clearing** - the column reads loud for exactly as
/// long as there is news in it, which is not the case a permanent mark would
/// have to survive.
///
/// Still plain text, which is settled and the contrast fix does not reopen: an
/// accent here would make one hue mean "either something wants you or something
/// happened", the always-on accent glyph's defect one level up. And if one of the two carriers
/// ever has to go, **keep the word** - a different word is legible at any
/// brightness, which is what the measurement showed.
/// The words a folded project's row carries about itself: `1 failed \u{b7} 1
/// running \u{b7} 2 finished`. Pure - unit-tested.
///
/// **The collapsed row's whole contract is that it answers for rows it does not
/// draw**, and it used to have two words for that and no third - so a
/// project folded over a run that had fallen over said nothing at all about it,
/// while the chip put that same run on the top step of the scale. The worst
/// place to be silent.
///
/// **`failed` leads.** The row reads left to right and the order is triage
/// order, the same order the popover lists in; it is also the only ordering
/// that survives being skimmed, because somebody scanning a column of folded
/// projects for trouble should find it in a fixed column position rather than
/// after a variable-width `running` tally.
///
/// **Three words, two kinds, and the difference is not cosmetic.** `finished`
/// is a **news** tally: it counts sessions the reader has not looked at and it
/// disappears when they do. `running` and `failed` are **state** tallies - a
/// failed run is still failed after you have read about it - so a folded row
/// goes on saying `1 failed` after it has stopped saying `1 finished`. That is
/// the row behaving exactly like the rail row above it: word clears, state
/// stays.
///
/// **And the tallies may not overlap.** A failed unread session's news word
/// *is* `failed`, so it is not also counted under `finished`: one session may
/// never occupy two of the row's three words, or the folded row would overstate
/// the project and repeat, one line up, the very contradiction of a chip and
/// a row disagreeing about one run. So `finished` counts unread sessions that **did not fail**.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FoldedTally {
    pub(crate) failed: usize,
    pub(crate) waiting: usize,
    pub(crate) running: usize,
    pub(crate) finished: usize,
}

impl FoldedTally {
    /// Count `threads` for a folded row. Each session lands on **one** word,
    /// its highest: `failed`, then `waiting`, then `running`, then unread
    /// `finished`. `Starting` is not counted - it claims nothing by design,
    /// and a tally is a claim.
    ///
    /// `with_waiting` is the one difference between the two folded rows. A
    /// folded project leaves waiting to the title bar's chip and counts such
    /// a session under whatever else it is; a folded group answers for
    /// several projects at once and says `waiting` itself.
    pub(crate) fn of<'a>(
        threads: impl IntoIterator<Item = &'a crate::project::Thread>,
        with_waiting: bool,
    ) -> Self {
        use crate::project::ThreadStatus;
        let mut tally = Self::default();
        for thread in threads {
            match thread.status {
                ThreadStatus::Failed => tally.failed += 1,
                ThreadStatus::WaitingForInput if with_waiting => tally.waiting += 1,
                ThreadStatus::Thinking => tally.running += 1,
                _ if thread.finished_unseen.is_some() => tally.finished += 1,
                _ => {}
            }
        }
        tally
    }
}

pub(crate) fn folded_project_tally_words(
    failed: usize,
    running: usize,
    finished: usize,
) -> (Option<String>, Option<String>, Option<String>) {
    (
        (failed > 0).then(|| format!("{failed} failed")),
        (running > 0).then(|| format!("{running} running")),
        (finished > 0).then(|| format!("{finished} finished")),
    )
}

/// That tally, drawn - or nothing when the project has neither to report.
///
/// `running` sits one step fainter than `finished` (`faint` under
/// `text_tertiary`), which is the requirement that keeps them from reading as
/// one thing: work in progress is a state, and a finished run is unread news,
/// and news outranks a state. The separator takes the fainter tone because it
/// belongs to neither word.
fn folded_project_tally(
    failed: usize,
    running: usize,
    finished: usize,
    ui: crate::theme::UiColors,
) -> Option<gpui::AnyElement> {
    let (failed_word, running_word, finished_word) =
        folded_project_tally_words(failed, running, finished);
    // Each word carries the tone its own mark carries one line up: the failure
    // colour for `failed`, `running` a step fainter than the news it sits
    // beside, and `finished` the news tone itself.
    let words: Vec<(String, gpui::Hsla)> = [
        failed_word.map(|word| (word, ui.agent_error)),
        running_word.map(|word| (word, ui.faint)),
        finished_word.map(|word| (word, ui.text_tertiary)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if words.is_empty() {
        return None;
    }
    let mut row = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::XS)
        .font_family(tok::font::MONO)
        .text_size(tok::mono::HINT);
    for (index, (word, tone)) in words.into_iter().enumerate() {
        if index > 0 {
            // The separator belongs to neither word, so it takes the fainter
            // tone rather than either one's.
            row = row.child(div().flex_none().text_color(ui.faint).child("\u{b7}"));
        }
        row = row.child(
            div()
                .flex_none()
                .text_color(tone)
                .child(SharedString::from(word)),
        );
    }
    Some(row.into_any_element())
}

/// The word an unread row shows for **how its run ended**.
///
/// One home for the mapping, because the rail row and the folded project's
/// tally both have to answer it the same way, and because a scale that grows a
/// sixth terminal word later should have one place to grow it.
pub(crate) fn news_word(status: crate::project::ThreadStatus) -> NewsWord {
    match status {
        crate::project::ThreadStatus::Failed => NewsWord::Failed,
        _ => NewsWord::Finished,
    }
}

/// An unread row's news, which is the outcome it is carrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewsWord {
    Finished,
    Failed,
}

impl NewsWord {
    fn label(self) -> &'static str {
        match self {
            NewsWord::Finished => "finished",
            NewsWord::Failed => "failed",
        }
    }

    /// **Every word has a colour**, so the failure word takes the
    /// failure colour. Not the accent: accent means *something wants you*, and
    /// a failed run wants nothing until the reader decides it does.
    fn tone(self, ui: crate::theme::UiColors) -> gpui::Hsla {
        match self {
            NewsWord::Finished => ui.text_tertiary,
            NewsWord::Failed => ui.agent_error,
        }
    }
}

/// The news word, in the muted plain-text voice both marks share.
///
/// **The word clears on being read; the dot does not.** Unread-ness is about
/// the reader and ends when they look; failure is about the session and
/// survives being looked at. So a read failed row is a bright-enough name, a
/// red dot and the slot back to what the session *is* - the news is spent, the
/// state remains.
fn surface_row_news(word: NewsWord, ui: crate::theme::UiColors) -> gpui::AnyElement {
    div()
        .flex_none()
        // Stated rather than inherited, because the folded project's tally
        // calls this from a row that does not set it. One home for the voice is
        // the point: the two marks say the same thing and drifted apart once
        // already, when only this one was measured.
        .font_family(tok::font::MONO)
        .text_size(tok::mono::HINT)
        .text_color(word.tone(ui))
        .child(word.label())
        .into_any_element()
}

/// The Ø6 status dot the design puts at the right edge of a surface row,
/// under the name it is exported by. See [`status_dot`].
pub(crate) fn surface_status_dot(
    status: crate::project::ThreadStatus,
    ui: crate::theme::UiColors,
) -> gpui::AnyElement {
    status_dot("slot-header", Some(status), false, None, ui)
}

/// The Ø6 status dot the design puts at the right edge of a surface row.
/// `None` - a shell or a view - has no turn to be in, so it takes the idle
/// treatment rather than no dot at all: a missing dot would shift the column.
///
/// # Two shapes, and what they say
///
/// "starting" is drawn as a **ring** rather than a fifth fill. The
/// settled states are filled dots; the one unsettled state is the one that is
/// not filled. At 6px a ring is still tellable from a fill in peripheral
/// vision, which is the only place these are ever read, and it needs no new
/// colour to be told apart from `idle`'s dimmed grey.
///
/// The attention scale spends that same shape a second time and for the same reason. **An
/// uncollected result stands on the reader exactly as a waiting agent does**,
/// so it takes the accent; what tells the two apart is the shape, not the
/// brightness:
///
/// > A waiting agent is standing because it needs an answer; a finished one is
/// > standing because its result has been accepted by nobody. "Just done" would
/// > assume an agent's output can be trusted unread; an uncollected finish is
/// > unchecked work, not a closed task.
///
/// So: **filled means the session is standing by itself, hollow means there is
/// something here to accept.** `starting` keeps its own ring in `muted`, which
/// is a different tone answering a different question, and the two never occur
/// on one row - a run that has finished is not starting.
///
/// If the ring ever turns out not to read on a given renderer, the designer's
/// fallback is stated and is **size, not fill**: a filled 6px against a filled
/// 4px. Worse, and legible everywhere. The 1.5px border is a floor either way -
/// it is the first thing rounding eats.
fn status_dot(
    id: &'static str,
    status: Option<crate::project::ThreadStatus>,
    unseen: bool,
    announcing: Option<crate::app::announce::AnnouncePhase>,
    ui: crate::theme::UiColors,
) -> gpui::AnyElement {
    use crate::app::announce::{DotFill, announceable_dot};
    use crate::project::ThreadStatus;
    let fill = match status {
        Some(ThreadStatus::Starting) => DotFill::Ring(ui.muted),
        // "running": the agent's turn is in flight.
        Some(ThreadStatus::Thinking) => DotFill::Solid(ui.vc_modified),
        // "waiting": the turn stopped on the user.
        Some(ThreadStatus::WaitingForInput) => DotFill::Solid(ui.accent),
        Some(ThreadStatus::Failed) => DotFill::Solid(ui.agent_error),
        // The run is over and honestly `idle` - and there is something here to
        // accept, which is the accent's own question.
        Some(ThreadStatus::Idle) | None if unseen => DotFill::Ring(ui.accent),
        Some(ThreadStatus::Idle) | None => DotFill::Solid(ui.faint.opacity(0.5)),
    };
    announceable_dot(
        SharedString::from(id),
        RAIL_STATUS_DOT,
        fill,
        ui.accent,
        announcing,
    )
}

/// The container row's disclosure caret: one glyph that turns 90° instead of
/// two glyphs that swap. The rotation is the design's only stated motion
/// besides the skeleton pulse, and [`tok::motion::CARET`] is its duration.
///
/// Re-keying the animation id on `is_expanded` is what makes it run on the
/// state change: GPUI restarts an animation when its id changes, so opening
/// plays 0° → 90° and closing plays the way back.
///
/// **It is the only thing that folds a project, and it changes nothing else.**
/// The row it sits in selects; this toggles. Splitting them is what makes
/// "fold that project away" a gesture a user can make while working in another
/// one - it used to move the content area onto the project being folded, which
/// is the opposite of what the gesture asks for.
fn disclosure_caret(
    ws_idx: usize,
    project_id: u64,
    is_expanded: bool,
    ui: crate::theme::UiColors,
    cx: &mut Context<SplitlaneApp>,
) -> gpui::AnyElement {
    let (from, to) = if is_expanded {
        (0.0, 0.25)
    } else {
        (0.25, 0.0)
    };
    div()
        .id(SharedString::from(format!("rail-caret-hit-{project_id}")))
        .flex_none()
        .w(RAIL_CARET_WIDTH)
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        // Both halves, which is the pattern every other nested control in this
        // rail uses: the press is what the row's drag and its click-count both
        // start from, and the release is what would select the project.
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
            this.close_agents_menu(cx);
            this.commit_agents_rename(cx);
            if let Some(project) = this.workspaces.get_mut(ws_idx) {
                project.is_expanded = !project.is_expanded;
            }
            this.save_session(cx);
            cx.stop_propagation();
            cx.notify();
        }))
        .child(
            svg()
                .size(tok::mono::LABEL)
                .flex_none()
                .path("icons/chevron-right.svg")
                // `svg()` paints in its own element's colour or not at all.
                .text_color(ui.faint)
                .with_animation(
                    SharedString::from(format!("rail-caret-{project_id}-{is_expanded}")),
                    Animation::new(tok::motion::CARET),
                    move |svg, delta| {
                        svg.with_transformation(Transformation::rotate(percentage(
                            from + (to - from) * delta,
                        )))
                    },
                ),
        )
        .into_any_element()
}

/// A surface that runs a plain shell, not a CLI agent. Terminal-kind threads
/// carry `terminal_agent: None` exactly when nothing was launched into them -
/// which is what the bottom dock's tabs used to be, and what the container's
/// shell surfaces are now.
/// Whether anything is open over the rail that a tooltip must not cover.
///
/// GPUI owns tooltips and the app owns menus, so nothing else settles the
/// z-order: a tooltip drawn while a menu is open lands on top of it, and the
/// entry it covers is the one under the pointer - the one the menu was opened
/// to pick. Asked once, here, because this has now been fixed three times in
/// three places (the container row's path, the armed confirm, and the `\u{22ef}`
/// button's own "More actions") and each fix knew about a different subset.
pub(crate) fn rail_overlay_open(app: &SplitlaneApp) -> bool {
    app.agents_view.agents_menu_open.is_some()
        || app.agents_view.agents_confirm_delete.is_some()
        || app.workspace_menu_open.is_some()
        || app.tab_menu_open.is_some()
        // A rename puts a text field where the row's label was; a tooltip on
        // top of the field the user is typing into is the same defect wearing
        // a different overlay.
        || app.agents_view.agents_rename_input.is_some()
}

pub(crate) fn is_shell_surface(thread: &crate::project::Thread) -> bool {
    thread.kind == crate::project::ThreadKind::Terminal && thread.terminal_agent.is_none()
}

/// Whether a slot holds nothing - the launcher, which is not in this list.
///
/// The rail is the inventory of what exists; an empty pane names no surface
/// and holds no work, which is also why restore drops it.
fn pane_is_empty(pane: &gpui::Entity<crate::Pane>, cx: &gpui::App) -> bool {
    let pane = pane.read(cx);
    pane.showing_launcher() && pane.tabs.is_empty()
}

/// Whether a slot in the tree is showing the container's diff - which has the
/// `Changes` row of its own.
fn pane_shows_diff(pane: &gpui::Entity<crate::Pane>, cx: &gpui::App) -> bool {
    let pane = pane.read(cx);
    matches!(
        pane.tabs.get(pane.selected_idx),
        Some(crate::pane::TabContent::Diff(_))
    )
}

/// The container's pane that is showing its diff, if one is.
/// The pane that *holds* the diff, whether or not it is the tab on top.
///
/// A surface behind a tab keeps its rail row, so the `Changes` row
/// needs the pane even when something has been raised over the diff - that is
/// where clicking the row has to go.
fn pane_holding_diff(
    container: &crate::workspace::Workspace,
    cx: &gpui::App,
) -> Option<gpui::Entity<crate::Pane>> {
    container
        .root
        .as_ref()?
        .collect_leaves()
        .into_iter()
        .find(|pane| {
            pane.read(cx)
                .tabs
                .iter()
                .any(|tab| matches!(tab, crate::pane::TabContent::Diff(_)))
        })
}

fn pane_showing_diff(
    container: &crate::workspace::Workspace,
    cx: &gpui::App,
) -> Option<gpui::Entity<crate::Pane>> {
    container
        .root
        .as_ref()?
        .collect_leaves()
        .into_iter()
        .find(|pane| pane_shows_diff(pane, cx))
}

/// Whether a slot in the tree is showing an agent surface - which already has
/// a row of its own, under the container it is parked on.
fn pane_shows_agent_surface(pane: &gpui::Entity<crate::Pane>, cx: &gpui::App) -> bool {
    pane.read(cx)
        .active_terminal_opt()
        .is_some_and(|view| view.read(cx).agent_thread_id.is_some())
}

/// Section eyebrow - a small uppercase muted label introducing a
/// rail section. When `add_button_id` is `Some`, a trailing `+` opens the
/// folder picker - and at the root of the rail "create" has exactly one
/// answer, so the button needs no menu. The caller passes an
/// already-uppercased label so this stays a pure layout helper.
fn section_eyebrow(
    label: &str,
    add_button_id: Option<SharedString>,
    ui: crate::theme::UiColors,
    cx: &mut Context<SplitlaneApp>,
) -> gpui::AnyElement {
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .px(tok::space::XXL)
        .py(tok::space::SM)
        .child(
            crate::ui_primitives::section_eyebrow(label.to_string(), ui)
                .flex_1()
                .min_w_0()
                .truncate(),
        );
    if let Some(id) = add_button_id {
        let hover_background = crate::app::constants::sidebar_tab_active_background();
        let resting_background = hover_background.opacity(0.0);
        row = row.child(
            div()
                .id(id)
                .role(Role::Button)
                .aria_label("New project")
                .flex_none()
                .size(tok::space::SECTION)
                .flex()
                .items_center()
                .justify_center()
                .rounded(tok::radius::BADGE)
                .bg(resting_background)
                .text_color(ui.muted)
                .animated_hover_element(move |button, delta| {
                    let icon_color = lerp_color(ui.muted, ui.text, delta);
                    button
                        .style()
                        .bg(lerp_color(resting_background, hover_background, delta))
                        .text_color(icon_color);
                    button.extend([svg()
                        .size(tok::space::XL)
                        .flex_none()
                        .path("icons/plus.svg")
                        .text_color(icon_color)
                        .into_any_element()]);
                })
                .tooltip(crate::ui_primitives::text_tooltip("New project"))
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                    this.create_agents_project_with_picker(cx);
                })),
        );
    }
    row.into_any_element()
}

/// Compact inline hint shown under the PROJECTS eyebrow when no
/// project exists yet. The eyebrow's `+` is the create affordance; this is
/// just guidance copy.
fn projects_empty_hint(ui: crate::theme::UiColors) -> impl IntoElement {
    div()
        .px(tok::space::MD)
        .py(tok::space::XS)
        .text_size(tok::text::CAPTION)
        .text_color(ui.muted)
        .child("No projects yet. Click + to add one.")
}

/// The design's "no sessions yet" line, drawn at surface-row indent so the
/// empty container still shows where its rows will land.
fn empty_project_hint(ui: crate::theme::UiColors) -> impl IntoElement {
    div()
        .ml(RAIL_SURFACE_MARGIN_L)
        .pl(RAIL_SURFACE_INDENT)
        .pr(RAIL_ROW_PADDING_X)
        .h(tok::row::SESSION)
        .flex()
        .items_center()
        .font_family(tok::font::MONO)
        .text_size(tok::mono::ROW)
        .text_color(ui.faint)
        .child("no sessions yet")
}

/// The `×` on the `Changes` row: closes the container's diff surface.
fn close_surface_button(
    id: SharedString,
    ws_idx: usize,
    tooltips: bool,
    ui: crate::theme::UiColors,
    cx: &mut Context<SplitlaneApp>,
) -> gpui::AnyElement {
    let resting_background = crate::app::constants::sidebar_tab_active_background().opacity(0.0);
    let hover_background = crate::app::constants::sidebar_tab_active_background();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label("Close")
        .flex_none()
        .size(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(tok::radius::TAG)
        .bg(resting_background)
        .animated_hover_bg(resting_background, hover_background)
        .when(tooltips, |b| {
            b.tooltip(crate::ui_primitives::text_tooltip("Close"))
        })
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
            this.close_container_diff_surface(ws_idx, cx);
            cx.stop_propagation();
        }))
        .child(
            svg()
                .size(px(16.))
                .flex_none()
                .path("icons/close.svg")
                .text_color(ui.muted),
        )
        .into_any_element()
}

/// The order the rail emits one container's surface rows in.
///
/// Indices into the container's own `threads`, so everything downstream -
/// `select_thread`, `remove_thread`, the drag payload - still resolves to the
/// row it names however the rows are grouped.
pub(crate) struct SurfaceRowOrder {
    /// Agent surfaces: pinned first, then the rest, newest first inside each.
    pub agents: Vec<usize>,
    /// Shells, newest first. The design puts them under the container's views.
    pub shells: Vec<usize>,
}

/// Group a container's surfaces the way the design orders them, and raise the
/// pinned ones inside their group.
///
/// This was written inline in the render loop and got the pin backwards.
/// Unpinned rows went straight into the list as the loop found them, while
/// pinned ones had to be collected - a pinned row cannot be emitted until the
/// loop has seen every row it must rise above - so they were appended
/// afterwards and a pin moved a surface to the **bottom** of its project. They
/// were reversed on the way out as well, so two pinned rows came back
/// oldest-first while every other group in this rail is newest-first.
pub(crate) fn surface_row_order(threads: &[crate::project::Thread]) -> SurfaceRowOrder {
    let mut pinned = Vec::new();
    let mut agents = Vec::new();
    let mut shells = Vec::new();
    // Newest first within each group: the container appends, so the tail is
    // the most recent.
    for thread_idx in (0..threads.len()).rev() {
        let thread = &threads[thread_idx];
        if thread.pinned {
            // Pinning used to lift a surface into a PINNED section of its own,
            // above every container. The design's rail is one list, so a pin
            // raises the surface to the top of the container it belongs to -
            // the same promise, without a second place for one row to live.
            // A pinned shell rises among the agents, because the pin is the
            // stronger claim: it is the one the user made.
            pinned.push(thread_idx);
        } else if is_shell_surface(thread) {
            shells.push(thread_idx);
        } else {
            agents.push(thread_idx);
        }
    }
    pinned.extend(agents);
    SurfaceRowOrder {
        agents: pinned,
        shells,
    }
}

/// The actions a rail row carries under the pointer: pin, delete, overflow.
///
/// `is_pinned` is `None` for a row that cannot be pinned - a container - and
/// then no pin button is emitted. Everything else is shared, deliberately: a
/// container row that offered a *similar* cluster would drift from this one,
/// and the reason the container had no delete affordance at all was that its
/// only path was a right-click into a menu that silently scrolled.
///
/// `id_key` disambiguates the element ids; it is the surface id for a session
/// row and the container id for a container row.
/// How much room [`hover_actions_cluster`] needs, in the row's own flow.
///
/// The cluster is absolute, so nothing laid out in the row knows it is there:
/// the title is `flex_1` and, once the meta slot fades, it grew straight under
/// the trash and the `\u{22ef}` instead of truncating before them. A long
/// session name read through the buttons sitting on top of it. The fading slot
/// keeps this width instead of collapsing, so the title truncates exactly where
/// the buttons begin.
///
/// Three 20px boxes with two 4px gaps, or two of them when there is no pin.
/// The cluster's own 8px inset is the row's right padding, so it does not count
/// twice. Callers scale it by the hover progress, so a resting row keeps every
/// pixel of its title and only gives ground while the buttons are coming in.
fn hover_actions_reserve(is_pinned: Option<bool>) -> gpui::Pixels {
    let buttons = if is_pinned.is_some() { 3.0 } else { 2.0 };
    px(buttons * 20.0 + (buttons - 1.0) * 4.0)
}

fn hover_actions_cluster(
    target: crate::project::AgentsTarget,
    id_key: u64,
    is_pinned: Option<bool>,
    tooltips: bool,
    ui: crate::theme::UiColors,
    cx: &mut Context<SplitlaneApp>,
) -> gpui::Div {
    let thread_id = id_key;
    let hover_background = crate::app::constants::sidebar_tab_active_background();
    let resting_background = hover_background.opacity(0.0);
    let pinned = is_pinned.unwrap_or(false);
    // An svg, not a `★` / `☆` glyph. The glyph was set at `text::BODY`
    // beside a 16px trash, and a text glyph's ink is about half its em: it
    // measured **7 x 7pt against the trash's 12 x 14**, in the same 20px box,
    // a hairline apart. Reported from real use, then measured. Raising the
    // font size cannot close it - the ink would need a ~24px em, whose line
    // box does not fit a 20px button - so the two neighbours are now the same
    // kind of thing at the same size, which is also the rule this codebase
    // already states: 16 for a glyph that IS the whole click target.
    let pin_icon = if pinned {
        "icons/star-filled.svg"
    } else {
        "icons/star.svg"
    };
    let pin_tooltip = if pinned { "Unpin" } else { "Pin" };
    let pin_resting_text = if pinned { ui.accent } else { ui.muted };
    let pin_btn = div()
        .id(SharedString::from(format!("agents-thread-{thread_id}-pin")))
        .role(Role::Button)
        .aria_label(pin_tooltip)
        .flex_none()
        .w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(tok::radius::BADGE)
        .bg(resting_background)
        .text_color(pin_resting_text)
        // The icon is emitted from inside the hover closure for the reason the
        // trash below states at length: an `svg()` paints in its OWN style's
        // colour and inherits nothing, so a child with no `text_color` renders
        // as nothing at all while the button, its tooltip and its handler all
        // keep working.
        .animated_hover_element(move |button, delta| {
            let icon_color = lerp_color(pin_resting_text, ui.text, delta);
            button
                .style()
                .bg(lerp_color(resting_background, hover_background, delta))
                .text_color(icon_color);
            button.extend([svg()
                .size(px(16.))
                .flex_none()
                .path(pin_icon)
                .text_color(icon_color)
                .into_any_element()]);
        })
        .when(tooltips, |b| {
            b.tooltip(crate::ui_primitives::text_tooltip(pin_tooltip))
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
            this.toggle_pin_for_target(target, cx);
            cx.stop_propagation();
        }));

    // The row's overflow menu had no
    // mouse path at all - right-click was the only way in, and the title-bar
    // `⋯` that used to dispatch `OpenAgentsThreadMenu` was unreachable chrome
    // removed in `fdaa764`. This button is the temporary home: the design
    // moves the menu into the slot header, which does not exist yet.
    //
    // An svg at the same 16px as its two neighbours. It was a text glyph -
    // "Splitlane ships no ellipsis asset, and the glyph sidesteps the `svg()`
    // colour trap entirely" - which was true and cost the row a third weight:
    // measured against the trash beside it, its ink was 8.5pt wide to the
    // trash's 12. Shipping the asset is cheaper than three buttons that read
    // as three sizes.
    //
    // The slot header's own ellipsis stays a glyph on purpose: measured, it
    // puts 8.5pt of ink beside a close mark that puts 6, so there is nothing
    // to close there and an asset would be a change with no reader.
    let overflow_btn = div()
        .id(SharedString::from(format!(
            "agents-thread-{thread_id}-overflow"
        )))
        .role(Role::Button)
        .aria_label("More actions")
        .flex_none()
        .w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(tok::radius::BADGE)
        .bg(resting_background)
        .text_color(ui.muted)
        .animated_hover_element(move |button, delta| {
            let icon_color = lerp_color(ui.muted, ui.text, delta);
            button
                .style()
                .bg(lerp_color(resting_background, hover_background, delta))
                .text_color(icon_color);
            button.extend([svg()
                .size(px(16.))
                .flex_none()
                .path("icons/dots.svg")
                .text_color(icon_color)
                .into_any_element()]);
        })
        .when(tooltips, |b| {
            b.tooltip(crate::ui_primitives::text_tooltip("More actions"))
        })
        .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
            if let Some(position) = e.mouse_position() {
                this.commit_agents_rename(cx);
                this.open_agents_menu_for_target(target, position, MenuOrigin::RailRow, cx);
            }
            cx.stop_propagation();
        }));

    let trash_btn = div()
        .id(SharedString::from(format!(
            "agents-thread-{thread_id}-trash"
        )))
        .role(Role::Button)
        .aria_label("Delete")
        .flex_none()
        .w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(tok::radius::BADGE)
        .bg(resting_background)
        .text_color(ui.muted)
        // The icon is emitted from inside the hover closure, exactly like the
        // "New project" button above. `svg()` paints a monochrome mask in its
        // OWN style's text color and does not inherit the parent's, so an
        // `svg()` child with no `text_color` of its own renders as nothing:
        // the button, its tooltip and its click target all work while the
        // glyph is simply invisible. Re-emitting it per frame is also what
        // lets the icon animate with the button instead of staying flat.
        .animated_hover_element(move |button, delta| {
            let icon_color = lerp_color(ui.muted, ui.text, delta);
            button
                .style()
                .bg(lerp_color(resting_background, hover_background, delta))
                .text_color(icon_color);
            button.extend([svg()
                .size(px(16.))
                .flex_none()
                .path("icons/trash.svg")
                .text_color(icon_color)
                .into_any_element()]);
        })
        .when(tooltips, |b| {
            b.tooltip(crate::ui_primitives::text_tooltip("Delete"))
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
            // The same confirm the row's menu opens. It used to arm an inline
            // red "Delete" in the row instead, so one object had two different
            // confirms depending on which door you came through - and only the
            // dialog could say what a delete takes with it ("and its 9
            // sessions"), which a 20px pill in a 272px rail cannot.
            this.request_delete_for_target(target, cx);
            cx.stop_propagation();
        }));

    div()
        .absolute()
        .top(px(0.))
        .bottom(px(0.))
        .right(px(8.))
        .flex()
        .flex_row()
        .items_center()
        // Zed `gap_1` = 4px (4px grid base unit).
        .gap(tok::space::XS)
        .children(is_pinned.is_some().then_some(pin_btn))
        .child(trash_btn)
        .child(overflow_btn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_renaming_target_maps_rename_enum_to_unified_target() {
        use crate::project::AgentsTarget;
        let p_target = AgentsTarget::Thread {
            ws_idx: 1,
            thread_idx: 2,
        };

        // A surface rename matches only its exact row.
        let renaming_thread = Some(AgentsRenameTarget::Thread {
            ws_idx: 1,
            thread_idx: 2,
        });
        assert!(is_renaming_target(renaming_thread, p_target));
        assert!(!is_renaming_target(
            renaming_thread,
            AgentsTarget::Thread {
                ws_idx: 1,
                thread_idx: 5
            }
        ));

        // A container rename never matches a surface row (it targets the
        // header, not a row).
        let renaming_project = Some(AgentsRenameTarget::Project { ws_idx: 1 });
        assert!(!is_renaming_target(renaming_project, p_target));

        // No active rename -> nothing matches.
        assert!(!is_renaming_target(None, p_target));
    }

    /// The container's surfaces in creation order: two agents, a shell, a
    /// third agent. Titles carry the assertion so a failure names the row.
    fn container_surfaces() -> Vec<crate::project::Thread> {
        use crate::agent_launcher::TerminalAgent;
        use crate::project::Thread;
        vec![
            Thread::new_terminal("oldest agent", "/w", Some(TerminalAgent::ClaudeCode)),
            Thread::new_terminal("middle agent", "/w", Some(TerminalAgent::ClaudeCode)),
            Thread::new_terminal("a shell", "/w", None),
            Thread::new_terminal("newest agent", "/w", Some(TerminalAgent::ClaudeCode)),
        ]
    }

    fn titles(threads: &[crate::project::Thread], order: &[usize]) -> Vec<String> {
        order.iter().map(|i| threads[*i].title.clone()).collect()
    }

    #[test]
    fn surfaces_are_newest_first_and_shells_sit_in_their_own_group() {
        let threads = container_surfaces();
        let order = surface_row_order(&threads);
        assert_eq!(
            titles(&threads, &order.agents),
            ["newest agent", "middle agent", "oldest agent"]
        );
        assert_eq!(titles(&threads, &order.shells), ["a shell"]);
    }

    #[test]
    fn a_pin_raises_a_surface_to_the_top_of_its_project() {
        // The whole point of the pin, and the direction it had backwards: the
        // pinned row was appended after every unpinned one, so pinning the
        // oldest agent moved it from the bottom of the group to... the bottom
        // of the group, under the rows it was supposed to have risen above.
        let mut threads = container_surfaces();
        threads[0].pinned = true;
        let order = surface_row_order(&threads);
        assert_eq!(
            titles(&threads, &order.agents),
            ["oldest agent", "newest agent", "middle agent"]
        );
    }

    #[test]
    fn two_pinned_surfaces_keep_the_rail_newest_first() {
        // The second half of the same defect: the pinned group was reversed on
        // the way out, so it came back oldest-first while every other group in
        // this rail is newest-first.
        let mut threads = container_surfaces();
        threads[0].pinned = true;
        threads[3].pinned = true;
        let order = surface_row_order(&threads);
        assert_eq!(
            titles(&threads, &order.agents),
            ["newest agent", "oldest agent", "middle agent"]
        );
    }

    #[test]
    fn a_pinned_shell_rises_with_the_agents() {
        // The pin is the stronger claim: it is the one the user made, and a
        // pinned row that stayed in the shells group below the views would not
        // be at the top of anything.
        let mut threads = container_surfaces();
        threads[2].pinned = true;
        let order = surface_row_order(&threads);
        assert_eq!(order.agents.first().copied(), Some(2));
        assert!(order.shells.is_empty());
    }

    /// Trouble first, then work, then news - and a third that is zero says
    /// nothing at all.
    #[test]
    fn a_folded_project_names_trouble_then_work_then_news() {
        assert_eq!(folded_project_tally_words(0, 0, 0), (None, None, None));
        assert_eq!(
            folded_project_tally_words(0, 1, 0),
            (None, Some("1 running".to_string()), None)
        );
        assert_eq!(
            folded_project_tally_words(0, 0, 2),
            (None, None, Some("2 finished".to_string()))
        );
        // **`failed` leads**, because the row is read left to right and the
        // order is triage order - and because somebody scanning a column of
        // folded projects for trouble should find it in a fixed column
        // position, not after a variable-width `running` tally.
        assert_eq!(
            folded_project_tally_words(1, 1, 2),
            (
                Some("1 failed".to_string()),
                Some("1 running".to_string()),
                Some("2 finished".to_string())
            )
        );
        assert_eq!(
            folded_project_tally_words(3, 0, 0),
            (Some("3 failed".to_string()), None, None)
        );
    }

    /// The row's three words are two kinds, and the one that is news names the
    /// **outcome** rather than the fact that a run ended.
    #[test]
    fn the_news_word_names_how_the_run_ended() {
        use crate::project::ThreadStatus;
        assert_eq!(news_word(ThreadStatus::Idle), NewsWord::Finished);
        assert_eq!(news_word(ThreadStatus::Failed), NewsWord::Failed);
        // A run still in flight has no outcome to carry; the mark is only ever
        // set on a run that ended, so this is the harmless default rather than
        // a claim.
        assert_eq!(news_word(ThreadStatus::Thinking), NewsWord::Finished);
    }
}
