//! Pane - a tabbed container holding one or more views (terminals or markdown
//! viewers, freely mixed within the same tab strip).
//!
//! Each leaf in the split tree holds an `Entity<Pane>`. A Pane manages an
//! ordered list of [`TabContent`] tabs and a single `selected_idx` cursor.
//! Markdown tabs and terminal tabs share the strip - the user opens markdown
//! files by clicking the doc icon (or Cmd/Ctrl-clicking a `.md` path inside a
//! terminal), and a new tab is appended to the same pane rather than splitting.
//!
//! Communication with the parent (split tree owner) uses the Zed pattern:
//! Pane emits `PaneEvent` via `cx.emit()`, parent subscribes via `cx.subscribe()`.
//!
//! Tab bar UI is modeled after Zed's tab bar design.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Context, DragMoveEvent, Entity,
    EventEmitter, FocusHandle, Focusable, Hsla, InteractiveElement, IntoElement, MouseButton,
    Pixels, Point, Render, SharedString, Size, Styled, Window, deferred, div, ease_out_quint,
    prelude::*, px, rgb, svg,
};

use crate::settings::components::with_alpha;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};

use crate::diff::DiffView;
use crate::file_view::FileView;
use crate::pane_drag::{
    DropEdge, MarkdownFileDrag, RailSurfaceDrag, SPLIT_EDGE_BAND, SessionDrag, compute_drop_edge,
    split_rect,
};
use crate::terminal::{TerminalEvent, TerminalView};
use crate::ui_tokens as tok;

// ---------------------------------------------------------------------------
// TabContent - a tab can hold either a terminal or a markdown viewer
// ---------------------------------------------------------------------------

/// A single tab inside a pane. Terminal and markdown tabs share the strip so
/// the user keeps tab navigation (Ctrl+Tab, click) regardless of content type
/// opening a markdown file from a terminal pane appends a tab next to the
/// existing terminals rather than splitting the layout.
#[derive(Clone)]
pub enum TabContent {
    Terminal(Entity<TerminalView>),
    Markdown(Entity<FileView>),
    Diff(Entity<DiffView>),
}

impl TabContent {
    pub fn as_terminal(&self) -> Option<&Entity<TerminalView>> {
        match self {
            TabContent::Terminal(t) => Some(t),
            TabContent::Markdown(_) | TabContent::Diff(_) => None,
        }
    }

    /// Stable identity of the tab's backing entity, regardless of variant.
    /// Lets per-tab click closures re-resolve their live index by
    /// identity when the `Vec` mutates between render and click.
    pub fn entity_id(&self) -> gpui::EntityId {
        match self {
            TabContent::Terminal(t) => t.entity_id(),
            TabContent::Markdown(m) => m.entity_id(),
            TabContent::Diff(d) => d.entity_id(),
        }
    }
}

/// What the slot header knows about a surface that the surface's own PTY
/// cannot say: which kind it is, which model it is talking to, what turn it is
/// in, and which branch its container is on.
///
/// Pushed rather than read, like every other app-owned slot on a `Pane`
/// (the attention maps, the composer slot): a `Pane` never
/// reaches into app state. Keyed by the surface's terminal entity, because an
/// agent surface can move between slots and panes and its identity has to
/// travel with the process rather than with the box it is in.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SurfaceFacts {
    /// Whether this surface runs a CLI coding agent. Not the same question as
    /// "does it have a thread id": a bare shell created from the rail is a
    /// surface record too, and it is the record's `terminal_agent` that
    /// separates the two.
    pub is_agent: bool,
    /// The model an agent surface is talking to, in the form the badge shows
    /// it ("opus 4.6"). `None` for a shell, and for an agent whose transcript
    /// has not named one yet - a session that has not replied once.
    pub model: Option<SharedString>,
    /// The turn an agent surface is in, from its surface record - the same
    /// field the rail's row reads. `None` for a shell, which has no turn to be
    /// in and takes neither a word nor a dot.
    pub status: Option<crate::project::ThreadStatus>,
    /// The branch the surface's container is on, and whether that checkout is
    /// a linked git worktree. `None` outside a repository, which is the
    /// design's rule: a header with nothing to say about git says nothing.
    pub branch: Option<(SharedString, bool)>,
    /// What this agent surface has in context, and the ceiling it will compact
    /// at when that has been observed. `None` for a shell and for a session
    /// that has not replied once.
    pub context: Option<ContextFact>,
}

/// The context meter's two numbers, in the two phases the design gives it.
///
/// **The ceiling is measured, not assumed.** Nothing on disk states the context
/// window and it belongs to the plan rather than the model, so the only honest
/// denominator is the size this session's own automatic compaction fired at.
/// Until one has, there is no share to draw - which is phase one, a count
/// named as a count - and after one, there is, which is the arc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextFact {
    pub tokens: u64,
    pub ceiling: Option<u64>,
    /// The newest compaction, when one has been seen. `turn_since == false` is
    /// what puts the ring on the arc.
    pub compaction: Option<crate::claude_sessions::CompactionSeen>,
    /// How many compactions this app has watched. A floor, not a total.
    pub compactions_seen: u32,
}

impl ContextFact {
    /// How full, `0.0..=1.0`, once the ceiling is known.
    pub fn fraction(self) -> Option<f32> {
        let ceiling = self.ceiling.filter(|c| *c > 0)?;
        Some((self.tokens as f32 / ceiling as f32).clamp(0.0, 1.0))
    }

    /// A token count in the unit that suits it: `683k`, `1.2M`.
    fn amount(tokens: u64, in_millions: bool) -> String {
        if in_millions {
            format!("{:.2}M", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1_000 {
            format!("{}k", tokens / 1_000)
        } else {
            tokens.to_string()
        }
    }

    /// What the header prints beside the arc, at the rung it has room for.
    ///
    /// Four cells, and the `ctx` at the end of three of them is doing work
    /// rather than decorating: **the short phase-two form is the only one with
    /// an arc beside it**, and an arc says what the number is about. The others
    /// may be read alone.
    ///
    /// The full form states both numbers in **one unit**, chosen by the
    /// ceiling, so a reader compares two figures rather than converting
    /// between them.
    ///
    /// It is a count and it looks like one - no per cent sign, no track -
    /// because before the ceiling is measured there is nothing to be a share
    /// of, and a number that looks like a share while being a count is the
    /// failure this meter was withdrawn for once already.
    pub fn header_label(self, full: bool) -> String {
        match self.ceiling.filter(|c| *c > 0) {
            None => format!("{} ctx", Self::amount(self.tokens, false)),
            Some(_) if !full => Self::amount(self.tokens, false),
            Some(ceiling) => {
                let millions = ceiling >= 1_000_000;
                format!(
                    "{} / {} ctx",
                    Self::amount(self.tokens, millions),
                    Self::amount(ceiling, millions)
                )
            }
        }
    }

    /// What the pointer gets, which is **what the header cannot say**.
    ///
    /// Not the share: that is already drawn by the arc and printed by the
    /// fraction. The exact numbers, where the ceiling came from, and how many
    /// times this session has compacted - which is not decoration either, it
    /// is what explains the sawtooth to somebody who saw the drop before they
    /// read any of this.
    pub fn tooltip_lines(self) -> Vec<String> {
        let mut lines = vec![format!("{} in context", thousands(self.tokens))];
        match self.ceiling.filter(|c| *c > 0) {
            Some(ceiling) => {
                lines.push(format!("ceiling {}", thousands(ceiling)));
                lines.push("where this session last compacted on its own".to_string());
            }
            None => lines.push("no ceiling until this session compacts on its own".to_string()),
        }
        if self.compactions_seen > 0 {
            lines.push(match self.compactions_seen {
                1 => "compacted once".to_string(),
                n => format!("compacted {n} times"),
            });
        }
        if let Some(seen) = self.compaction.filter(|c| !c.turn_since) {
            lines.push(format!(
                "compacted {} · {} → {}",
                crate::app::agents_sidebar::limits_footer::clock_hhmm(
                    (seen.at as u64).saturating_mul(1000)
                ),
                Self::amount(seen.pre_tokens, seen.pre_tokens >= 1_000_000),
                Self::amount(seen.post_tokens, false),
            ));
        }
        lines
    }

    /// Whether the ring is on: a compaction with nothing added since.
    pub fn just_compacted(self) -> bool {
        self.compaction.is_some_and(|c| !c.turn_since)
    }
}

/// `683044` as `683 044`. The tooltip has room for the exact figure, and an
/// exact figure nobody can read at a glance is not exact where it counts.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// Outer size of the context arc. Not text, so it takes no rung of the header's
/// ladder and is drawn at every width - including the narrowest pane the app
/// will make, which is where four panes are open and the meter is wanted most.
const CONTEXT_ARC: f32 = 11.0;
/// How thick the ring is. The hole is what makes it read as a dial rather than
/// a pie at eleven pixels.
const CONTEXT_ARC_STROKE: f32 = 2.5;
/// Steps the sweep is drawn in. At this size the eye cannot tell an arc from a
/// 32-gon, and a path of straight segments needs no curve maths to be right.
const CONTEXT_ARC_STEPS: usize = 32;

/// The context meter: a ring filled clockwise from twelve o'clock.
///
/// **Brightness is the reading here, and that is allowed.** The rule elsewhere
/// in this app is to spend hue, shape or presence before brightness, because
/// brightness is what it uses for rank; this is not two states being told apart
/// but one continuous quantity, and the design gives it three steps - `dim`
/// while there is room, `text_tertiary` past the middle, `text` when it is
/// nearly full. The accent is not among them: it answers "does something want
/// me", and a session filling its context wants nothing.
fn context_arc(fill: f32, just_compacted: bool, ui: crate::theme::UiColors) -> impl IntoElement {
    let colour = if fill >= 0.85 {
        ui.text
    } else if fill >= 0.5 {
        ui.text_tertiary
    } else {
        ui.dim
    };
    let track = ui.border;
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let centre = bounds.center();
            let outer = px(CONTEXT_ARC / 2.0);
            let inner = px(CONTEXT_ARC / 2.0 - CONTEXT_ARC_STROKE);
            let ring = |from: f32, to: f32, colour: gpui::Hsla, window: &mut gpui::Window| {
                if to <= from {
                    return;
                }
                let mut builder = gpui::PathBuilder::fill();
                let at = |turn: f32, radius: gpui::Pixels| {
                    // Twelve o'clock, clockwise: the direction a person reads a
                    // dial, and the direction the number grows.
                    let angle = (turn * std::f32::consts::TAU) - std::f32::consts::FRAC_PI_2;
                    gpui::point(
                        centre.x + radius * angle.cos(),
                        centre.y + radius * angle.sin(),
                    )
                };
                let steps = ((to - from) * CONTEXT_ARC_STEPS as f32).ceil().max(1.0) as usize;
                builder.move_to(at(from, outer));
                for step in 1..=steps {
                    builder.line_to(at(from + (to - from) * step as f32 / steps as f32, outer));
                }
                for step in (0..=steps).rev() {
                    builder.line_to(at(from + (to - from) * step as f32 / steps as f32, inner));
                }
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, colour);
                }
            };
            // The track first and whole, so the reading is the difference
            // between two rings rather than an arc floating on the pane.
            ring(0.0, 1.0, track, window);
            ring(0.0, fill.clamp(0.0, 1.0), colour, window);
            // The ring says one thing - **that drop was a compaction, not a
            // fall in spending** - so it is drawn while the two can still be
            // confused and comes off with the next turn that puts something
            // back. A hairline outside the track, which is the one place on an
            // eleven-pixel dial not already carrying the reading.
            if just_compacted {
                let mut halo = gpui::PathBuilder::stroke(px(1.0));
                let radius = outer + px(1.5);
                let at = |turn: f32| {
                    let angle = turn * std::f32::consts::TAU;
                    gpui::point(
                        centre.x + radius * angle.cos(),
                        centre.y + radius * angle.sin(),
                    )
                };
                halo.move_to(at(0.0));
                for step in 1..=CONTEXT_ARC_STEPS {
                    halo.line_to(at(step as f32 / CONTEXT_ARC_STEPS as f32));
                }
                if let Ok(path) = halo.build() {
                    window.paint_path(path, ui.border_hover);
                }
            }
        },
    )
    .w(px(CONTEXT_ARC))
    .h(px(CONTEXT_ARC))
    .flex_none()
}

// ---------------------------------------------------------------------------
// Tab bar color helpers - derived from active theme
// ---------------------------------------------------------------------------

fn tab_colors() -> crate::theme::UiColors {
    crate::theme::ui_colors()
}

/// The fill behind a slot's header. Two of the three signals the design
/// gives focus are colours, and this is one of them: the header lifts by a
/// step when the slot takes input. Windows terminal material still wins, so
/// the strip and the terminal below it share one native backdrop.
fn tab_bar_background(
    ui: &crate::theme::UiColors,
    terminal_material_active: bool,
    focused: bool,
) -> Hsla {
    if terminal_material_active && cfg!(target_os = "windows") {
        gpui::transparent_black()
    } else if focused {
        ui.slot_header_focus
    } else {
        ui.slot_header
    }
}

fn pane_content_background(
    theme: &crate::theme::TerminalTheme,
    terminal_material_active: bool,
    terminal_selected: bool,
) -> Hsla {
    if !terminal_material_active || !terminal_selected {
        return theme.background;
    }

    #[cfg(target_os = "windows")]
    {
        gpui::transparent_black()
    }

    #[cfg(not(target_os = "windows"))]
    {
        theme.background
    }
}

/// First line of an agent question, bounded for the collapsed peek badge.
/// Pure - unit-tested below.
fn peek_badge_line(message: &str) -> String {
    const BADGE_MAX_CHARS: usize = 80;
    let first = message.lines().next().unwrap_or("").trim();
    let mut line: String = first.chars().take(BADGE_MAX_CHARS).collect();
    if first.chars().count() > BADGE_MAX_CHARS {
        line.push('…');
    }
    line
}

/// Bar height. This bar *is* the slot header - the strip over a slot saying
/// what it shows and carrying what can be done to it - so it is the header's
/// height, not a second one that happens to look similar.
const TAB_BAR_HEIGHT: Pixels = crate::app::slot_header::SLOT_HEADER_HEIGHT;

/// Gap between adjacent chips in the strip.
const STRIP_GAP: f32 = 4.0;
/// Fixed chip width. Longer labels get truncated with ellipsis inside this box.
const TAB_WIDTH: f32 = 140.0;
/// Approximate title capacity inside `TAB_WIDTH` after the leading slot, gaps,
/// and horizontal padding. Above this, the CSS ellipsis is expected to engage.
const TAB_TITLE_TOOLTIP_THRESHOLD: usize = 13;
/// Section padding (start/end areas of the bar).
const SECTION_PX: f32 = 4.0;
/// The steps of the design's pane-header priority ladder, named for
/// the item each one lets in. See `Pane::header_level`.
const HEADER_LEVEL_STATUS: u8 = 1;
const HEADER_LEVEL_RERUN: u8 = 2;
/// The widest step: the badge is the last item the ladder lets in. It was named
/// `HEADER_LEVEL_METER` for the context meter, which the rendered face fed, and
/// then `HEADER_LEVEL_WIDEST` for the permission bar's "no timer here" note.
/// Both are gone, so the step above the badge had nothing left to let in and
/// the ladder is four steps rather than five.
const HEADER_LEVEL_BADGE: u8 = 3;
/// The square inside the Stop button - the whole button below 420px.
const STOP_GLYPH: Pixels = px(7.);
/// Square size shared by tab-bar icon buttons.
const ACTION_BUTTON_SIZE: f32 = 22.0;
/// Full-distance duration for every tab-bar hover transition.
const TAB_BAR_HOVER_MS: u64 = 120;
/// Square size of the new-tab affordance that trails the last tab chip.
/// Plus glyph size inside the new-tab affordance.
/// Stroke width (px) of the broadcast-group stripe on the pane's left edge.
/// Dashed at this width GPUI draws a 4px dash and a 2px gap, which is what the
/// design's "dashed accent, 2px" resolves to.
const BROADCAST_STRIPE_WIDTH: f32 = 2.0;
/// Width (px) of the box that carries the stripe. Wider than the stroke on
/// purpose - see the comment at the stripe's own render site.
const BROADCAST_STRIPE_BOX: f32 = 6.0;
/// Uniform gap (px) between the drop-to-split preview overlay and its region's
/// edges, so the blue box floats inside the target half/pane.
const OVERLAY_MARGIN: f32 = 8.0;
/// Corner radius (px) of the drop-to-split preview overlay.
const OVERLAY_RADIUS: Pixels = tok::radius::PANEL;
/// Apple system blue (#007AFF), used for the CLI drop placement preview.
const DROP_OVERLAY_BLUE: u32 = 0x007aff;
/// Low-alpha fill so the placement card stays visible without washing the pane.
const DROP_OVERLAY_BACKGROUND_ALPHA: f32 = 0.10;
/// Hard upper bound on tab title length in characters. Mirrors Zed's
/// `MAX_TAB_TITLE_LEN` (`zed/crates/editor/src/items.rs:64`). Anything past
/// this is replaced with a trailing ellipsis before the CSS ellipsis layer.
const MAX_TAB_TITLE_LEN: usize = 24;

/// Char-boundary-safe `truncate_and_trailoff`. Counts chars (not bytes) so
/// filenames with multibyte UTF-8 (accents, CJK, emoji) don't trigger a
/// byte-index panic, and reserves one char for the trailing `…`.
fn truncate_tab_title(raw: &str) -> String {
    if raw.chars().count() <= MAX_TAB_TITLE_LEN {
        return raw.to_string();
    }
    let head: String = raw.chars().take(MAX_TAB_TITLE_LEN - 1).collect();
    format!("{head}…")
}

// ---------------------------------------------------------------------------
// Pane events - emitted to parent via cx.emit()
// ---------------------------------------------------------------------------

pub enum PaneEvent {
    /// The last tab was closed - parent should remove this pane from the split tree.
    Remove,
    /// A parked agent surface was dropped onto this slot from the rail. The
    /// container and the surface, not the position: the tree mutation and the
    /// PTY mount belong to `SplitlaneApp`, and emitting defers them out of the
    /// drop callback (entity re-entrancy, as with every other drop here).
    DropRailSurface { ws_idx: usize, thread_id: u64 },
    /// A tile of the launcher was picked. The pane cannot create a session -
    /// it knows its `workspace_id` and not its directory, and the thread record
    /// belongs to the container - so it names the agent and `SplitlaneApp`
    /// creates it into this same pane, which is what `FOCUS.md` asks for:
    /// "Picking an agent in it fills that same pane; focus does not move,
    /// because it was already there."
    /// A launcher row was taken - by click or by `enter`. The pane names what
    /// was picked; the app is the only thing that knows what a container is,
    /// so it does the filling.
    LauncherPick(crate::app::launcher::LauncherAction),
    /// The `\u{d7}` at the right end of the slot header: close this whole slot,
    /// with everything in it.
    ///
    /// Distinct from [`PaneEvent::Remove`], which reports that a pane emptied
    /// itself. This one is a decision, so the parent records it on the
    /// undo-close stack before acting on it.
    CloseSlot,
    /// Request a fresh terminal tab in this pane. Routed to `SplitlaneApp` (not
    /// handled in the `Pane`) so the new terminal spawns at the owning
    /// workspace's cwd - the `Pane` knows only its `workspace_id`, not the
    /// directory - and gets the app-level CWD/port/service subscription wired,
    /// exactly like the other drops. Without this, a new tab on
    /// Windows opened in the process `current_dir()` (`C:\Program Files\Splitlane`).
    /// A surface's custom name changed via inline rename - the parent
    /// should persist the session so the name survives restart.
    SurfaceRenamed {
        /// The surface's record, when it has one. Every terminal in a pane
        /// does; a markdown or diff tab does not, and neither is renamable.
        thread_id: Option<u64>,
        /// The name the user typed, or `None` when they cleared it and the
        /// auto-derived title takes over again.
        name: Option<String>,
        /// What the header shows now that the write has landed. For a clear
        /// this is the derived title the record has to fall back to: lifting
        /// the lock without it left the rail drawing the name that had just
        /// been erased.
        display: String,
    },
    /// What this pane is showing changed without changing the layout tree. The app persists this because pane-local mutations can
    /// otherwise be lost on crash.
    TabsChanged,
    /// The slot header's overflow button was pressed: the menu for the
    /// surface this pane is showing. Named for the tab strip it used to
    /// hang off; the strip is gone and the menu is the surface's own.
    OpenTabMenu {
        tab_id: gpui::EntityId,
        position: Point<Pixels>,
    },
    /// An agent-session row was dropped out of the sessions sidebar onto this
    /// pane (bridges the sessions sidebar and pane drag-and-drop). The
    /// parent spawns a *fresh* terminal at `cwd` running the agent's resume
    /// command, then - for `edge = Some` - splits this (the emitting target)
    /// pane toward that edge, or - for `edge = None` (center) - appends it as a
    /// new tab here. Routed to `SplitlaneApp` because spawning a terminal needs
    /// the app-level CWD/port subscription wiring (mirrors `DropSplit`).
    DropSessionSplit {
        edge: Option<DropEdge>,
        agent: crate::agent_sessions::SessionAgent,
        session_id: String,
        cwd: String,
    },
    /// A markdown file was dropped out of the Files sidebar onto this (the
    /// emitting target) pane.
    /// For `edge = Some` the parent opens the file in a new pane split toward
    /// that edge; for `edge = None` (center) it appends the markdown as a new
    /// tab here. Routed to `SplitlaneApp` (LayoutTree owner) to keep the tree
    /// mutation out of the drop callback (entity re-entrancy, mirrors
    /// `DropSessionSplit`).
    DropMarkdownSplit {
        edge: Option<DropEdge>,
        path: std::path::PathBuf,
    },
}

/// Inline surface-rename state.
struct TabRename {
    /// Index of the tab being renamed.
    idx: usize,
    /// The field itself - the same [`TextArea`](crate::widgets::text_area::TextArea)
    /// the rail's rename uses, and the reason this is an entity rather than a
    /// `String`.
    ///
    /// It used to be a bare buffer driven by an `on_key_down` arm that pushed
    /// `key_char` and popped on backspace, and **explicitly dropped every
    /// keystroke carrying `control` or `platform`** - so the one gesture a
    /// person reaches for when renaming a session, paste, was refused by name.
    /// It had no cursor either (the caret was a literal `|` at the end), no
    /// selection, no arrows, no delete, and no IME. Reported from live use as
    /// "renaming a session does not support Cmd+V".
    ///
    /// One rename affordance, one input widget: the widget already answers all
    /// of that, on all three platforms, and its bindings are re-registered on
    /// every `apply_keybindings` so a config reload cannot degrade it.
    input: Entity<crate::widgets::text_area::TextArea>,
}

/// Reversible hover transition state for one tab-bar interactive surface.
struct TabBarHoverMotion {
    /// Last progress painted by the animator, used to seed mid-flight reversals.
    live_progress: Rc<Cell<f32>>,
    from: f32,
    target: f32,
    /// Restarts GPUI's one-shot animation whenever the hover target changes.
    epoch: u64,
}

impl TabBarHoverMotion {
    fn new(live_progress: Rc<Cell<f32>>) -> Self {
        Self {
            live_progress,
            from: 0.0,
            target: 0.0,
            epoch: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Pane - tabbed terminal container
// ---------------------------------------------------------------------------

pub struct Pane {
    pub tabs: Vec<TabContent>,
    pub selected_idx: usize,
    /// Terminals of this pane whose agent
    /// session is `WaitingForInput`, with the agent's question (≤512 chars,
    /// UNTRUSTED display-only text). Pushed by `SplitlaneApp::sync_attention`
    /// recomputed from the session truth on every transition, never
    /// mutated locally. Drives the attention ring, the tab dot and the peek
    /// overlay.
    attention: std::collections::HashMap<gpui::EntityId, Option<String>>,
    /// Terminals of this pane whose agent
    /// session is `Errored` (the agent binary exited non-zero). Pushed by
    /// `SplitlaneApp::sync_attention` alongside `attention` - same idempotent
    /// recompute-from-session-truth contract. Drives the dedicated
    /// `agent_error` tab dot (tab-anatomy state slot, ranked above the
    /// waiting dot).
    errored: std::collections::HashSet<gpui::EntityId>,
    /// Transient fleet-grep match counts per
    /// terminal. Pushed by `SplitlaneApp::push_fleet_badges` after a fan-out,
    /// cleared 4 s later or when the fleet overlay closes. The
    /// LOWEST-priority tab adornment - first to yield its slot.
    search_hits: std::collections::HashMap<gpui::EntityId, usize>,
    /// The peek badge is hovered - render the full question panel.
    peek_expanded: bool,
    /// Set to true when the workspace is zoomed on this pane.
    pub zoomed: bool,
    /// Workspace ID for spawning new terminals with correct env vars.
    /// What the app knows about each surface in this pane that the surface
    /// itself cannot say. Keyed by the surface's terminal entity; pushed once
    /// per frame by `SplitlaneApp::sync_surface_facts`.
    surface_facts: std::collections::HashMap<gpui::EntityId, SurfaceFacts>,
    /// Whether a context menu is open anywhere in the window, pushed once per
    /// frame beside the facts. See [`Self::set_menu_open`].
    menu_open: bool,
    /// The header's own width, written once per frame by a canvas in it.
    ///
    /// It is what the design's priority ladder is measured against
    /// (`Pane::header_level`): the header drops whole items by width rather
    /// than shrinking everything, and a pane's width is a layout outcome that
    /// nothing in `Pane` knows without measuring it.
    header_width: Rc<Cell<f32>>,
    /// Per-surface hover progress for reversible tab-bar fades and tint shifts.
    tab_bar_hover_motion: std::collections::HashMap<SharedString, TabBarHoverMotion>,
    /// Cached `splitlane.json` so `render_tab_bar` never calls the
    /// blocking `load_config()` per frame (the agent-button visibility gate and
    /// the launch command read it). Hydrated at creation, refreshed by
    /// `SplitlaneApp::process_config_changes` → `Workspace::propagate_config` on
    /// every `ConfigWatcher` reload, so a Settings flip (e.g. the Claude bypass
    /// toggle) takes effect on the next click without a per-frame disk read.
    pub cached_config: splitlane_config::schema::SplitlaneConfig,
    /// Inline tab-rename state. `None` when not renaming.
    rename: Option<TabRename>,
    /// Live drop-to-split target: the edge the blue overlay
    /// previews while a tab is dragged over this pane's content. `None` =
    /// center band (move-into-pane) or no drag. Updated by the content
    /// `on_drag_move` handler; reset on drop. While no drag is active the
    /// overlay is `invisible()` regardless of this value, so a stale value
    /// after a cancel is harmless (the next drag-move recomputes it).
    drag_split_direction: Option<DropEdge>,
    /// Previous drop region, kept only as a *fallback* start rect for the glide
    /// on the first crossing of a drag, before the live position cell
    /// ([`Self::overlay_current`]) holds anything meaningful. Set to the old
    /// value of `drag_split_direction` each time it changes.
    overlay_prev_dir: Option<DropEdge>,
    /// Start rect `(x, y, w, h)` of the current glide, captured at the instant
    /// the region changes. Captured from the overlay's *live* on-screen
    /// position ([`Self::overlay_current`]) rather than the previous region's
    /// resting rect, so a fast multi-band crossing redirects from wherever the
    /// box actually is mid-flight instead of jumping back to the prior target.
    overlay_from: (f32, f32, f32, f32),
    /// The overlay's live interpolated rect, written by the glide animator every
    /// frame and read back by `on_drag_move` to seed the next glide's start
    /// (see [`Self::overlay_from`]). `Rc<Cell>` because it is shared between the
    /// render-time animator closure and the event handler.
    overlay_current: Rc<Cell<(f32, f32, f32, f32)>>,
    /// Bumped every time `drag_split_direction` changes. Feeds the overlay's
    /// animation `ElementId`, so a new region restarts the glide from delta 0.
    overlay_seq: usize,
    /// Last observed content size (captured in the `on_drag_move` handler), used
    /// to convert a [`DropEdge`] into an absolute-pixel rectangle for the glide.
    overlay_pane_size: Size<Pixels>,
    /// The Composer overlay pushed by
    /// `SplitlaneApp::refresh_composer_slot` when this pane is the Composer
    /// target. `None` on every other pane. The pane renders it bottom-anchored
    /// and routes gestures back through the slot's closures - it never reads
    /// app state.
    composer_slot: Option<crate::app::composer::ComposerSlot>,
    /// Terminals of this pane holding a queued prompt
    /// (broadcast/Composer buffer awaiting the agent's next idle transition).
    /// Pushed by `SplitlaneApp::sync_pending_chips`; drives the "1 queued" tab
    /// chip.
    pending_prefill: std::collections::HashSet<gpui::EntityId>,
    /// Broadcast-group stripe color index (`UiColors::group_*`)
    /// when this pane is a group member. Pushed by
    /// `SplitlaneApp::sync_broadcast_stripes`. The stripe is a DISTINCT element
    /// from the attention border below - the pane border slot stays the glow's.
    broadcast_stripe: Option<usize>,
    /// This pane is showing the launcher over whatever it holds.
    ///
    /// A pane with no tabs shows the launcher unconditionally - that is what
    /// "an empty pane" means. This flag is the other half: the ladder's last
    /// rung reached a pane that is *not* empty, and the launcher takes it for
    /// as long as the choice is open. Nothing in the pane is closed by it, and
    /// picking an agent (or `Esc`) gives the pane straight back.
    launching: bool,
    /// The launcher's own focus target, so an empty pane is a pane in the one
    /// The launcher's filter field, focused the moment the launcher appears.
    ///
    /// Per pane rather than one on the app: two launcher panes can be on
    /// screen at once, and one field bound to whichever has focus would show
    /// the same text in both and clear the wrong one on a pick.
    /// The launcher's root, as a **dispatch anchor** rather than a place focus
    /// ever rests.
    ///
    /// GPUI builds a key-dispatch path out of focusable nodes; an ordinary
    /// `div` carrying `on_key_down` between the root and the focused element
    /// is not one, so the launcher's arrows and `enter` never arrived - the
    /// keys reached the field, which has no action for them, and stopped
    /// there. `focus_handle` still answers with the field: this handle is
    /// tracked, never focused. Measured 29 August 2026, by pressing `down`
    /// twice and watching the selection not move.
    launcher_anchor: FocusHandle,
    /// The list's scroll position, so a selection moved past the fold can be
    /// pulled back into view. Without it `\u{2193}` moves a highlight the user
    /// cannot see and `enter` takes a row they never read - the command
    /// palette solves the same problem the same way.
    launcher_scroll: gpui::ScrollHandle,
    /// The child index the list was last scrolled to, so the scroll happens
    /// when the selection moves and not on every frame. Scrolling per frame
    /// confiscates the wheel: in an app full of live terminals a redraw is
    /// always a moment away, and the list snapped back the instant the user
    /// tried to read past it. Found by a cross-vendor review.
    launcher_scrolled_to: Cell<Option<usize>>,
    launcher_query: Entity<crate::widgets::text_input::TextInput>,
    /// What can go in here, pushed once per frame by
    /// `SplitlaneApp::sync_launcher_rows`. The pane knows a `workspace_id` and
    /// not a directory, so it builds none of this and only draws it.
    launcher_rows: Vec<crate::app::launcher::LauncherRow>,
    /// Which visible row `enter` takes. An index into the *filtered* list, so
    /// every edit of the query resets it - the old cursor pointed into a
    /// different set of rows.
    launcher_selected: usize,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Pane {
    /// Create a new pane with a single terminal tab.
    pub fn new(terminal: Entity<TerminalView>, cx: &mut Context<Self>) -> Self {
        Self::subscribe_terminal(&terminal, cx);
        let cached_config = splitlane_config::loader::load_config();
        Self::apply_terminal_render_config(&terminal, &cached_config, cx);
        Self {
            tabs: vec![TabContent::Terminal(terminal)],
            selected_idx: 0,
            attention: std::collections::HashMap::new(),
            errored: std::collections::HashSet::new(),
            search_hits: std::collections::HashMap::new(),
            peek_expanded: false,
            zoomed: false,
            surface_facts: std::collections::HashMap::new(),
            menu_open: false,
            header_width: Rc::new(Cell::new(0.)),
            tab_bar_hover_motion: std::collections::HashMap::new(),
            // Hydrate the tab-bar config cache once at creation (not
            // per frame); refreshed on ConfigWatcher reload via propagation.
            cached_config,
            rename: None,
            drag_split_direction: None,
            overlay_prev_dir: None,
            overlay_from: (0.0, 0.0, 0.0, 0.0),
            overlay_current: Rc::new(Cell::new((0.0, 0.0, 0.0, 0.0))),
            overlay_seq: 0,
            overlay_pane_size: Size::default(),
            composer_slot: None,
            launching: false,
            launcher_anchor: cx.focus_handle(),
            launcher_scroll: gpui::ScrollHandle::new(),
            launcher_scrolled_to: Cell::new(None),
            launcher_query: new_launcher_query(cx),
            launcher_rows: Vec::new(),
            launcher_selected: 0,
            pending_prefill: std::collections::HashSet::new(),
            broadcast_stripe: None,
        }
    }

    /// Create a new pane wrapping an existing tab moved in from elsewhere
    /// (drop-to-split). The pane-level subscription is wired for a
    /// terminal tab so `ChildExited`/`TitleChanged` route here, but - unlike
    /// [`crate::SplitlaneApp::create_pane`] - the app-level terminal
    /// subscription is NOT re-added, because the moved terminal already has
    /// one from its original creation (re-adding would double CWD/port events).
    pub fn new_with_tab(tab: TabContent, cx: &mut Context<Self>) -> Self {
        Self::new_with_tabs(vec![tab], 0, cx)
    }

    pub fn new_with_tabs(
        tabs: Vec<TabContent>,
        selected_idx: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let cached_config = splitlane_config::loader::load_config();
        for tab in &tabs {
            if let TabContent::Terminal(t) = tab {
                Self::subscribe_terminal(t, cx);
                Self::apply_terminal_render_config(t, &cached_config, cx);
            }
        }
        let selected_idx = selected_idx.min(tabs.len().saturating_sub(1));
        // A pane holds one surface. The tab strip that could draw a second is
        // gone, so a pane built with two would hold a running PTY nothing can
        // show, select or close. Every caller trims first - restore through
        // `keep_one_surface_per_pane`, undo through a record of one - and this
        // is where a future one finds out it did not. Asserted rather than
        // typed because the vec is read in 22 files and collapsing it is a
        // separate cleanup; asserted rather than trusted because the last path
        // that appended was found by a reviewer, not by us.
        debug_assert!(
            tabs.len() <= 1,
            "a pane holds one surface; got {}",
            tabs.len()
        );
        Self {
            tabs,
            selected_idx,
            attention: std::collections::HashMap::new(),
            errored: std::collections::HashSet::new(),
            search_hits: std::collections::HashMap::new(),
            peek_expanded: false,
            zoomed: false,
            surface_facts: std::collections::HashMap::new(),
            menu_open: false,
            header_width: Rc::new(Cell::new(0.)),
            tab_bar_hover_motion: std::collections::HashMap::new(),
            // See `Pane::new`.
            cached_config,
            rename: None,
            drag_split_direction: None,
            overlay_prev_dir: None,
            overlay_from: (0.0, 0.0, 0.0, 0.0),
            overlay_current: Rc::new(Cell::new((0.0, 0.0, 0.0, 0.0))),
            overlay_seq: 0,
            overlay_pane_size: Size::default(),
            composer_slot: None,
            launching: false,
            launcher_anchor: cx.focus_handle(),
            launcher_scroll: gpui::ScrollHandle::new(),
            launcher_scrolled_to: Cell::new(None),
            launcher_query: new_launcher_query(cx),
            launcher_rows: Vec::new(),
            launcher_selected: 0,
            pending_prefill: std::collections::HashSet::new(),
            broadcast_stripe: None,
        }
    }

    /// Replace this pane's attention map
    /// (terminals whose agent waits for input + their question). Idempotent
    /// push from `SplitlaneApp::sync_attention` - repaints only on change.
    pub fn set_attention(
        &mut self,
        attention: std::collections::HashMap<gpui::EntityId, Option<String>>,
        cx: &mut Context<Self>,
    ) {
        if self.attention != attention {
            if attention.is_empty() {
                self.peek_expanded = false;
            }
            self.attention = attention;
            cx.notify();
        }
    }

    /// Replace this pane's Errored set
    /// (terminals whose agent binary exited non-zero). Same idempotent
    /// push contract as [`Pane::set_attention`] - repaints only on change.
    pub fn set_errored(
        &mut self,
        errored: std::collections::HashSet<gpui::EntityId>,
        cx: &mut Context<Self>,
    ) {
        if self.errored != errored {
            self.errored = errored;
            cx.notify();
        }
    }

    /// Replace this pane's transient fleet-grep
    /// badge counts. Same idempotent push contract as [`Pane::set_attention`].
    pub fn set_search_hits(
        &mut self,
        hits: std::collections::HashMap<gpui::EntityId, usize>,
        cx: &mut Context<Self>,
    ) {
        if self.search_hits != hits {
            self.search_hits = hits;
            cx.notify();
        }
    }

    /// Install/clear the Composer overlay on
    /// this pane. Always notifies - the slot carries live `busy`/group data
    /// recomputed by the pusher, and the closure fields defeat `PartialEq`.
    pub fn set_composer_slot(
        &mut self,
        slot: Option<crate::app::composer::ComposerSlot>,
        cx: &mut Context<Self>,
    ) {
        self.composer_slot = slot;
        cx.notify();
    }

    pub fn set_surface_facts(
        &mut self,
        facts: std::collections::HashMap<gpui::EntityId, SurfaceFacts>,
        cx: &mut Context<Self>,
    ) {
        if self.surface_facts == facts {
            return;
        }
        self.surface_facts = facts;
        cx.notify();
    }

    /// Tell this pane that a context menu is open somewhere in the window.
    ///
    /// It suppresses every tooltip the pane draws, and the rule behind that is
    /// one line: **a tooltip must never paint over a menu.** The two are drawn
    /// by different layers - GPUI owns the tooltip and shows it on hover after
    /// its own delay, the app owns the menu - so nothing stops the label of the
    /// button you just clicked from covering the list that click opened. Which
    /// is exactly what it did: `⋯` opened its menu and "More actions" sat on
    /// top of the first two rows of it.
    ///
    /// A pane-wide flag rather than a per-button one because the hazard is not
    /// about which button opened the menu. A menu is positioned at the pointer
    /// and can cover any neighbour, so any tooltip that would appear while one
    /// is open is a tooltip in the way.
    pub fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.menu_open == open {
            return;
        }
        self.menu_open = open;
        cx.notify();
    }

    /// The facts the app pushed for the surface this slot is showing.
    fn active_surface_facts(&self) -> Option<&SurfaceFacts> {
        let id = self.tabs.get(self.selected_idx)?.as_terminal()?.entity_id();
        self.surface_facts.get(&id)
    }

    /// Whether this pane is showing the launcher rather than a surface.
    ///
    /// A pane with no tabs always is - that is what "an empty pane" means, and
    /// it is the ladder's second rung. A pane that holds tabs shows it only
    /// while a choice is open over them.
    pub fn showing_launcher(&self) -> bool {
        self.tabs.is_empty() || self.launching
    }

    /// Take this frame's launcher rows. Cheap when nothing moved: the pane
    /// only redraws if the list it is holding actually differs.
    pub fn set_launcher_rows(
        &mut self,
        rows: Vec<crate::app::launcher::LauncherRow>,
        cx: &mut Context<Self>,
    ) {
        if self.launcher_rows == rows {
            return;
        }
        // Keep the row the user is on, named by what it does rather than by
        // where it sat. The pushed list changes under the cursor for reasons
        // that have nothing to do with the cursor - the history read landing,
        // a neighbouring shell reporting a new title - and snapping back to
        // the top between a `\u{2193}` and an `enter` fills the pane with a
        // row nobody chose. Found by a cross-vendor review.
        let anchor = self
            .visible_launcher_rows(cx)
            .get(self.launcher_selected)
            .map(|row| row.action.clone());
        self.launcher_rows = rows;
        self.launcher_selected = anchor
            .and_then(|action| {
                self.visible_launcher_rows(cx)
                    .iter()
                    .position(|row| row.action == action)
            })
            .unwrap_or(0);
        cx.notify();
    }

    /// Empty the filter, so the next launcher in this pane opens on the whole
    /// list rather than on someone's half-typed search.
    pub fn clear_launcher_query(&mut self, cx: &mut Context<Self>) {
        self.launcher_query.update(cx, |input, cx| input.clear(cx));
        self.launcher_selected = 0;
        cx.notify();
    }

    /// `↑` / `↓` / `enter` / `esc` in the launcher.
    ///
    /// Not in the design, which specifies the list but not its keys. Modelled
    /// on the command palette's handler so one list behaves like the other:
    /// the arrows clamp rather than wrap, `enter` takes the selected row, and
    /// anything else is an edit of the query and puts the cursor back on the
    /// first row.
    fn handle_launcher_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.showing_launcher() {
            return;
        }
        let len = self.visible_launcher_rows(cx).len();
        match event.keystroke.key.as_str() {
            "escape" => {
                if self.tabs.is_empty() {
                    // Nothing to give back: the pane itself goes, which is the
                    // whole of "dismissal costs nothing" - the launcher opened
                    // it and nothing was started in it.
                    cx.emit(PaneEvent::CloseSlot);
                } else {
                    self.launching = false;
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "enter" if len > 0 => {
                let idx = self.launcher_selected.min(len - 1);
                if let Some(action) = self
                    .visible_launcher_rows(cx)
                    .get(idx)
                    .map(|row| row.action.clone())
                {
                    cx.emit(PaneEvent::LauncherPick(action));
                }
                cx.stop_propagation();
            }
            "up" if len > 0 => {
                self.launcher_selected = self.launcher_selected.min(len - 1).saturating_sub(1);
                cx.notify();
                cx.stop_propagation();
            }
            "down" if len > 0 => {
                self.launcher_selected = (self.launcher_selected + 1).min(len - 1);
                cx.notify();
                cx.stop_propagation();
            }
            _ => {
                // An edit: the old index pointed into the previous, longer set.
                if self.launcher_selected != 0 {
                    self.launcher_selected = 0;
                    cx.notify();
                }
            }
        }
    }

    /// The rows this pane is showing right now: the pushed list, filtered by
    /// what is in the field.
    fn visible_launcher_rows(&self, cx: &App) -> Vec<&crate::app::launcher::LauncherRow> {
        let needle = self.launcher_query.read(cx).value().to_lowercase();
        self.launcher_rows
            .iter()
            .filter(|row| row.matches(&needle))
            .collect()
    }

    /// Open or close the launcher over this pane's tabs.
    pub fn set_launching(&mut self, launching: bool, cx: &mut Context<Self>) {
        if self.launching == launching {
            return;
        }
        self.launching = launching;
        // A launcher opening is a fresh question. Over a pane that holds a
        // surface this is the second, third, nth time the same pane has been
        // asked, and without this it opens on the half-typed filter and the
        // cursor left behind by the last one.
        if launching {
            self.launcher_query.update(cx, |input, cx| input.clear(cx));
            self.launcher_selected = 0;
        }
        cx.notify();
    }

    /// Replace the queued-prompt indicator set. Idempotent
    /// push from `SplitlaneApp::sync_pending_chips` - repaints only on change.
    pub fn set_pending_prefill(
        &mut self,
        pending: std::collections::HashSet<gpui::EntityId>,
        cx: &mut Context<Self>,
    ) {
        if self.pending_prefill != pending {
            self.pending_prefill = pending;
            cx.notify();
        }
    }

    /// Set/clear the broadcast-group stripe color slot.
    /// Idempotent push from `SplitlaneApp::sync_broadcast_stripes`.
    pub fn set_broadcast_stripe(&mut self, color_idx: Option<usize>, cx: &mut Context<Self>) {
        if self.broadcast_stripe != color_idx {
            self.broadcast_stripe = color_idx;
            cx.notify();
        }
    }

    /// "Send to agent" - a bottom-anchored line over a click-swallowing
    /// backdrop, so the terminal underneath receives neither keystrokes (the
    /// TextArea holds focus) nor clicks while it is up (theme-picker model).
    ///
    /// A redesign kept this and renamed it, and the rename is the point. The
    /// function was never "an input field outside the terminal" - it was
    /// "write to an agent without going to its pane", which is exactly what
    /// makes this app worth having at four sessions and has nothing to do with a
    /// rendered conversation. So it names its addressee, says where the text
    /// goes, and Enter sends.
    fn render_composer_overlay(&self, _cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let slot = self.composer_slot.clone()?;
        let ui = tab_colors();

        let mut header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .child(
                div()
                    .text_size(tok::text::CAPTION)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(ui.text)
                    .child("Send to agent"),
            )
            // Who it goes to. The one place in the app where the user writes
            // to a session without looking at it, so the session is named.
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::HINT)
                    .text_color(ui.text_tertiary)
                    .child(slot.addressee.clone()),
            );

        // Broadcast toggle: shows the active group when armed, plain label
        // otherwise. With no group defined the click routes to a toast that
        // points at the picker (handled app-side).
        let toggle = slot.toggle_broadcast.clone();
        let broadcast_label: SharedString = if slot.broadcast {
            match &slot.group_label {
                Some(label) => format!("Broadcast: {label}").into(),
                None => "Broadcast".into(),
            }
        } else {
            "Single pane".into()
        };
        let broadcast_bg = if slot.broadcast {
            ui.accent.opacity(0.15)
        } else {
            ui.subtle
        };
        let broadcast_text = if slot.broadcast { ui.accent } else { ui.muted };
        let broadcast_hover_text = if slot.broadcast { ui.accent } else { ui.text };
        header = header.child(
            div()
                .id("composer-broadcast-toggle")
                .px(tok::space::XS)
                .py(tok::space::XS)
                .rounded(tok::radius::BADGE)
                .text_size(tok::text::CAPTION)
                .bg(broadcast_bg)
                .text_color(broadcast_text)
                .animated_hover(move |style, delta| {
                    style.text_color(lerp_color(broadcast_text, broadcast_hover_text, delta));
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    toggle(cx);
                })
                .child(broadcast_label),
        );

        if slot.busy {
            // The target's agent
            // is generating - validation queues instead of delivering.
            header = header.child(
                div()
                    .px(tok::space::XS)
                    .py(tok::space::XS)
                    .rounded(tok::radius::BADGE)
                    .text_size(tok::text::CAPTION)
                    .bg(ui.vc_modified.opacity(0.15))
                    .text_color(ui.vc_modified)
                    .child("agent generating - Enter queues"),
            );
        }

        if slot.pending_count > 0 {
            let cancel = slot.cancel_pending.clone();
            header = header.child(
                div()
                    .id("composer-cancel-pending")
                    .px(tok::space::XS)
                    .py(tok::space::XS)
                    .rounded(tok::radius::BADGE)
                    .text_size(tok::text::CAPTION)
                    .bg(ui.subtle)
                    .text_color(ui.muted)
                    .animated_hover(move |style, delta| {
                        style.text_color(lerp_color(ui.muted, ui.vc_deleted, delta));
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cancel(cx);
                    })
                    .child(format!("{} queued · cancel", slot.pending_count)),
            );
        }

        // The explicit deliver-then-submit gesture is documented right on the
        // surface, and it is unavailable in broadcast.
        // The chord, and where the text lands. Broadcast still refuses to
        // submit, and says so: fanning a submit out to N sessions at once is
        // exactly the multi-target risk that refusal exists for, and this
        // overlay is not a broadcast tool in the design at all.
        let chord = if cfg!(target_os = "macos") {
            "\u{21e7}\u{2318}Space"
        } else {
            "Ctrl+Shift+Space"
        };
        let hint: SharedString = if slot.broadcast {
            format!("{chord} \u{b7} Enter pre-fills every ready member - broadcast never submits")
                .into()
        } else {
            format!("{chord} \u{b7} goes into the agent's terminal \u{b7} Esc keeps the draft")
                .into()
        };

        let dismiss_backdrop = slot.dismiss.clone();
        let dismiss_out = slot.dismiss.clone();
        Some(
            deferred(
                div()
                    .id("composer-backdrop")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .bg(gpui::hsla(0., 0., 0., 0.25))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        dismiss_backdrop(cx);
                    })
                    .child(
                        div()
                            .id("composer-panel")
                            .occlude()
                            .m(tok::space::MD)
                            .p(tok::space::MD)
                            .flex()
                            .flex_col()
                            .gap(tok::space::XS)
                            .bg(ui.overlay)
                            .border_1()
                            .border_color(ui.border)
                            .rounded(tok::radius::PANEL)
                            .shadow(crate::ui_primitives::menu_shadow(ui))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_mouse_down_out(move |_, _, cx| {
                                dismiss_out(cx);
                            })
                            .child(header)
                            .child(div().max_h(px(180.)).child(slot.input.clone()))
                            .child(
                                div()
                                    .text_size(tok::text::CAPTION)
                                    .text_color(ui.muted)
                                    .child(hint),
                            ),
                    ),
            )
            .with_priority(4)
            .into_any_element(),
        )
    }

    /// A compact badge on the pane showing the
    /// waiting agent's question without stealing focus; hover expands to the
    /// full message (≤512 chars, plain inert text - no links, no ANSI).
    /// Top-right under the tab bar so the agent's prompt line stays visible.
    fn render_peek_overlay(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.attention.is_empty() {
            return None;
        }
        // Prefer the visible tab's question; else the first waiting tab's.
        let message = self
            .tabs
            .get(self.selected_idx)
            .and_then(|t| t.as_terminal())
            .and_then(|t| self.attention.get(&t.entity_id()))
            .or_else(|| {
                self.tabs
                    .iter()
                    .filter_map(TabContent::as_terminal)
                    .find_map(|t| self.attention.get(&t.entity_id()))
            })
            .cloned()
            .flatten();
        let ui = tab_colors();
        let full = message.unwrap_or_else(|| "waiting for input".to_string());
        let shown = if self.peek_expanded {
            full
        } else {
            peek_badge_line(&full)
        };
        Some(
            div()
                .id(SharedString::from(format!(
                    "peek-{}",
                    cx.entity().entity_id().as_u64()
                )))
                .absolute()
                .top(TAB_BAR_HEIGHT + tok::space::SM)
                .right_2()
                .max_w(px(420.0))
                .overflow_hidden()
                .bg(ui.overlay)
                .border_1()
                .border_color(ui.vc_conflict.opacity(0.6))
                .rounded(tok::radius::SMALL)
                .px_2()
                .py_1()
                .text_xs()
                .text_color(ui.text)
                .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                    if this.peek_expanded != *hovered {
                        this.peek_expanded = *hovered;
                        cx.notify();
                    }
                }))
                .child(shown)
                .into_any_element(),
        )
    }

    /// skipped. Used by event handlers that need to scan terminals - sidebar
    /// counters, AI-tool PID owner lookups, layout serialization.
    pub fn terminals(&self) -> impl Iterator<Item = &Entity<TerminalView>> {
        self.tabs.iter().filter_map(TabContent::as_terminal)
    }

    pub fn apply_config(
        &mut self,
        config: &splitlane_config::schema::SplitlaneConfig,
        cx: &mut Context<Self>,
    ) {
        self.cached_config = config.clone();
        let terminals: Vec<Entity<TerminalView>> = self.terminals().cloned().collect();
        for terminal in terminals {
            Self::apply_terminal_render_config(&terminal, config, cx);
        }
        cx.notify();
    }

    fn apply_terminal_render_config(
        terminal: &Entity<TerminalView>,
        config: &splitlane_config::schema::SplitlaneConfig,
        cx: &mut Context<Self>,
    ) {
        let terminal_material_active = config.windows_terminal_material_enabled();
        let integrated_glyphs_enabled = config
            .terminal
            .as_ref()
            .is_none_or(|terminal| terminal.resolved_integrated_glyphs());
        let color_emoji_enabled = config
            .terminal
            .as_ref()
            .is_none_or(|terminal| terminal.resolved_color_emoji());
        let cursor_color_override = config
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.cursor_color.as_deref())
            .and_then(crate::terminal::view::hsla_from_hex_color);
        terminal.update(cx, |terminal, cx| {
            terminal.set_terminal_material_active(terminal_material_active, cx);
            terminal.set_integrated_glyphs_enabled(integrated_glyphs_enabled, cx);
            terminal.set_color_emoji_enabled(color_emoji_enabled, cx);
            terminal.set_cursor_color_override(cursor_color_override, cx);
        });
    }

    /// True when `terminal` is one of this pane's tabs.
    pub fn contains_terminal(&self, terminal: &Entity<TerminalView>) -> bool {
        self.terminals().any(|t| t == terminal)
    }

    /// Show `terminal` in this pane, in place of whatever was there.
    ///
    /// **It replaces rather than appends, and that is the model**: a
    /// pane holds one surface. A tab strip would answer "what do I have open" a
    /// second time, for one pane, while the rail already answers it for all of
    /// them - and two inventories that must agree eventually disagree.
    ///
    /// # Nothing is lost, and that is not a hope
    ///
    /// The displaced surface keeps running. Its `TerminalView` is **owned by
    /// `agents_terminal_view_cache`** and only *pointed at* from a pane, so
    /// dropping the pane's handle drops a clone, not the process; the cache's
    /// budget refuses to evict a terminal that has not exited, letting itself
    /// go over budget instead. So the surface stays in the rail, still running,
    /// one click from a pane again - which is what makes replacement cheap
    /// enough to be the ordinary case rather than the last rung.
    pub fn show_terminal(&mut self, terminal: Entity<TerminalView>, cx: &mut Context<Self>) {
        Self::subscribe_terminal(&terminal, cx);
        Self::apply_terminal_render_config(&terminal, &self.cached_config, cx);
        self.show_surface(TabContent::Terminal(terminal), cx);
    }

    /// The one way a surface enters a pane. See [`Self::show_terminal`] for why
    /// it replaces.
    pub fn show_surface(&mut self, content: TabContent, cx: &mut Context<Self>) {
        // The new surface goes in **before** the old ones are cleaned up, so
        // the pane is never observably empty. Nothing today could observe it -
        // GPUI owns entity state on one thread, and `clear_tab_hover_motion`
        // touches only the hover-animation map - but "a pane always holds a
        // surface" is an invariant worth holding by construction rather than by
        // an argument about threading that a future re-entrant call would
        // quietly invalidate. Raised by a cross-vendor pass as a race; it is
        // not one, and the ordering is free.
        let displaced = std::mem::take(&mut self.tabs);
        self.tabs.push(content);
        self.selected_idx = 0;
        for gone in displaced {
            self.clear_tab_hover_motion(gone.entity_id());
        }
        cx.emit(PaneEvent::TabsChanged);
        cx.notify();
    }

    /// Append a markdown viewer tab and focus it. Used by the doc-button
    /// handler in this pane's tab strip and by the Cmd/Ctrl-click flow on
    /// `.md` paths inside a terminal - both routes converge on this method
    /// via `SplitlaneApp::open_markdown_in_pane`.
    ///
    /// Markdown tabs don't need an event subscription: `FileView` does
    /// not emit pane-level events. Closing the tab through the tab strip's
    /// close button drops the entity, which in turn drops its file watcher.
    pub fn show_markdown(&mut self, markdown: Entity<FileView>, cx: &mut Context<Self>) {
        self.show_surface(TabContent::Markdown(markdown), cx);
    }

    /// Append a multi-worktree diff tab and select it. Like markdown tabs,
    /// `DiffView` emits no pane-level events, so no subscription is needed;
    /// closing the tab drops the entity (and any future watchers it owns).
    pub fn show_diff(&mut self, diff: Entity<DiffView>, cx: &mut Context<Self>) {
        self.show_surface(TabContent::Diff(diff), cx);
    }

    /// Subscribe to a terminal's events - close tab on exit, repaint on title change.
    fn subscribe_terminal(terminal: &Entity<TerminalView>, cx: &mut Context<Self>) {
        cx.subscribe(terminal, |this, terminal, event: &TerminalEvent, cx| {
            match event {
                TerminalEvent::ChildExited => {
                    if let Some(idx) = this
                        .tabs
                        .iter()
                        .position(|t| t.as_terminal() == Some(&terminal))
                    {
                        this.close_tab_at(idx, cx);
                    }
                }
                TerminalEvent::TitleChanged => {
                    cx.notify();
                }
                // CwdChanged, ActivityBurst, ServiceDetected, SelectionCopied are
                // handled by SplitlaneApp's direct subscription to each TerminalView.
                TerminalEvent::CwdChanged(_)
                | TerminalEvent::ActivityBurst
                | TerminalEvent::ServiceDetected(_)
                | TerminalEvent::CancelSwapMode
                | TerminalEvent::SelectionCopied
                | TerminalEvent::OpenMarkdownPath(_)
                | TerminalEvent::OpenCodePath { .. }
                | TerminalEvent::FontZoomChanged
                | TerminalEvent::FleetSearchRequested { .. } => {}
            }
        })
        .detach();
    }

    /// Get a display title for a tab. Markdown tabs use the file basename;
    /// terminal tabs detect well-known programs from the OSC title.
    ///
    /// Both variants are capped at 24 chars (Zed `MAX_TAB_TITLE_LEN`,
    /// `crates/editor/src/items.rs:64`). The CSS truncation chain
    /// (`min_w_0 + overflow_x_hidden + text_ellipsis`) on the title div
    /// is a second layer that catches edge cases - but Zed's experience is
    /// that flex layouts with `max_w` (no explicit `w`) sometimes fail to
    /// propagate the constraint, so capping the string up front is
    /// load-bearing for visual consistency. Without this, a long markdown
    /// filename like `doc-opencode-sessions.md` overflows the tab chip.
    fn tab_full_title(tab: &TabContent, cx: &App) -> String {
        match tab {
            TabContent::Markdown(md) => md.read(cx).title().to_string(),
            TabContent::Diff(d) => d.read(cx).title(),
            TabContent::Terminal(t) => Self::terminal_tab_full_title(t, cx),
        }
    }

    pub(crate) fn tab_title(tab: &TabContent, cx: &App) -> String {
        let raw = match tab {
            TabContent::Markdown(md) => md.read(cx).title().to_string(),
            TabContent::Diff(d) => d.read(cx).title(),
            TabContent::Terminal(t) => Self::terminal_tab_title(t, cx),
        };
        truncate_tab_title(&raw)
    }

    /// Icon path for a tab (rendered as a small leading SVG inside the tab
    /// chip). Differentiates terminal and markdown tabs at a glance.
    fn tab_icon(tab: &TabContent) -> &'static str {
        match tab {
            TabContent::Terminal(_) => "icons/terminal.svg",
            TabContent::Markdown(_) => "icons/file-text.svg",
            TabContent::Diff(_) => "icons/git-branch.svg",
        }
    }

    /// The design names two surfaces by a typographic mark rather than by an
    /// icon: `¶` for a rendered document, `±` for a diff. Splitlane ships
    /// neither as an asset, and a glyph sidesteps the `svg()` colour trap the
    /// way the rail's own `⋯` does. `None` keeps [`Self::tab_icon`].
    fn tab_glyph(tab: &TabContent, cx: &App) -> Option<&'static str> {
        match tab {
            TabContent::Terminal(_) => None,
            TabContent::Markdown(file) => Some(file.read(cx).glyph()),
            TabContent::Diff(_) => Some("±"),
        }
    }

    /// What a surface says about itself beside its name (the design: "badge =
    /// model for agents, 'not restorable' for shells").
    ///
    /// A rendered document keeps the design's "markdown · read only", which is
    /// the whole answer to "why is there no composer here". A shell says "not
    /// restorable", which is the design's own copy and is honest about what
    /// restore actually does with one: the surface comes back, at the same
    /// directory, holding neither the process that was running in it nor a
    /// line of its scrollback. An agent, whose conversation *is* restorable,
    /// says which model it is talking to instead.
    fn tab_badge(&self, tab: &TabContent, cx: &App) -> Option<SharedString> {
        match tab {
            TabContent::Markdown(file) => file.read(cx).badge(),
            TabContent::Terminal(_) => {
                let facts = self.active_surface_facts()?;
                if facts.is_agent {
                    facts.model.clone()
                } else {
                    Some(SharedString::from("not restorable"))
                }
            }
            TabContent::Diff(_) => None,
        }
    }

    /// The active surface's badge, drawn once for the slot rather than once per
    /// tab chip: it is a fact about what the slot is showing, and a strip of
    /// chips has no room to repeat it.
    ///
    /// One-surface slots only - which is the case the design draws, where the
    /// slot header is a name rather than a strip. Beside a strip the
    /// badge takes width the tabs need, and a clipped tab is a worse trade than
    /// an unstated "read only" the chip's own `¶` already implies.
    ///
    /// Level 3 of the priority ladder: below ~520px the badge is the
    /// first of the header's facts to go, before the branch and long before
    /// the name.
    fn render_surface_badge(
        &self,
        ui: crate::theme::UiColors,
        cx: &App,
    ) -> Option<gpui::AnyElement> {
        if self.tabs.len() != 1 || self.header_level() < HEADER_LEVEL_BADGE {
            return None;
        }
        let badge = self.tab_badge(self.tabs.get(self.selected_idx)?, cx)?;
        Some(
            div()
                .flex_none()
                .items_center()
                .px(tok::space::SM)
                .py(px(2.))
                .ml(tok::space::SM)
                .rounded(tok::radius::BADGE)
                .border_1()
                .border_color(ui.border)
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                // The same weight as the branch beside it. Both are facts
                // about the surface, read at a glance and never acted on, and
                // one step apart on the ramp they read as two ranks: `faint`
                // plus a border is the shape of a disabled chip, so the model
                // looked switched off next to the branch it sits beside.
                .text_color(ui.muted)
                .child(badge)
                .into_any_element(),
        )
    }

    fn terminal_tab_title(terminal: &Entity<TerminalView>, cx: &App) -> String {
        let view = terminal.read(cx);
        // A user-assigned custom name wins over the OSC-derived title
        // so a renamed tab visibly shows its new name.
        if let Some(custom) = view.terminal.custom_name.as_ref().filter(|c| !c.is_empty()) {
            return custom.clone();
        }
        let raw = &view.terminal.title;
        // An agent surface is named by its own title - the one its rail row
        // shows, kept current by the same OSC updates. Falling through to the
        // agent's product name below would give one surface two names
        // depending on where it is read, and a slot header exists to say what
        // the rail says.
        //
        // `surface_is_agent`, not `agent_thread_id.is_some()`: every terminal
        // in a pane has a record now, so having one stopped meaning "is an
        // agent". Asked the old way, a shell that gained a record was renamed
        // by the act of gaining it - which is why the adoption path used to
        // pin the displayed name as a custom one, freezing a label that had
        // been tracking the shell's cwd.
        if view.surface_is_agent
            && let Some(title) = crate::project::clean_sidebar_title(raw)
        {
            // Unless the process has said nothing yet and this is still the
            // placeholder every terminal starts on. Then the record's name is
            // the only real one either place has.
            if !Self::is_default_terminal_title(raw) {
                return title;
            }
            if let Some(from_record) = view
                .record_title
                .as_ref()
                .filter(|name| !name.trim().is_empty())
            {
                return from_record.clone();
            }
            return title;
        }
        if let Some(agent) = view.terminal.detected_agent {
            return agent.display_name().into();
        }
        // For shell titles like "user@host: /path/to/dir", extract the last path component
        if let Some(path_title) =
            Self::shell_path_title(raw).and_then(|path| Self::cwd_label(&path))
        {
            return path_title;
        }
        if let Some(agent_title) = Self::agent_title_from_terminal_title(raw) {
            return agent_title.into();
        }
        if Self::is_default_terminal_title(raw)
            && let Some(cwd) = view.terminal.current_cwd.as_deref()
            && let Some(label) = Self::cwd_label(cwd)
        {
            return label;
        }
        // Fallback: pass the raw title through. Length capping happens
        // uniformly in `tab_title` via `truncate_tab_title`, which counts
        // chars (not bytes) so multibyte UTF-8 stays sound.
        if raw.is_empty() {
            "Terminal".into()
        } else {
            raw.clone()
        }
    }

    fn terminal_tab_full_title(terminal: &Entity<TerminalView>, cx: &App) -> String {
        let view = terminal.read(cx);
        if let Some(custom) = view.terminal.custom_name.as_ref().filter(|c| !c.is_empty()) {
            return custom.clone();
        }
        let raw = &view.terminal.title;
        if let Some(agent) = view.terminal.detected_agent {
            return agent.display_name().into();
        }
        if let Some(path_title) = Self::shell_path_title(raw) {
            return path_title;
        }
        if let Some(agent_title) = Self::agent_title_from_terminal_title(raw) {
            return agent_title.into();
        }
        if Self::is_default_terminal_title(raw)
            && let Some(cwd) = view
                .terminal
                .current_cwd
                .as_ref()
                .filter(|cwd| !cwd.is_empty())
        {
            return cwd.clone();
        }
        if raw.is_empty() {
            "Terminal".into()
        } else {
            raw.clone()
        }
    }

    fn is_default_terminal_title(title: &str) -> bool {
        title.trim().is_empty() || title.trim().eq_ignore_ascii_case("terminal")
    }

    fn agent_title_from_terminal_title(title: &str) -> Option<&'static str> {
        let first = title.split_whitespace().next()?.trim();
        let first = first
            .strip_suffix(".exe")
            .or_else(|| first.strip_suffix(".EXE"))
            .unwrap_or(first);
        if let Some(agent) = crate::agent_launcher::TerminalAgent::from_binary(first) {
            return Some(agent.display_name());
        }
        match first.to_ascii_lowercase().as_str() {
            "nvim" | "neovim" => Some("Neovim"),
            "vim" => Some("Vim"),
            "top" | "htop" | "btop" => Some("System Monitor"),
            _ => None,
        }
    }

    fn shell_path_title(title: &str) -> Option<String> {
        let trimmed = title.rsplit(':').next()?.trim();
        if trimmed.starts_with('/') || trimmed.starts_with('~') {
            Some(trimmed.to_string())
        } else {
            None
        }
    }

    /// The name a directory gives a shell: its last component, or `~` for the
    /// home directory itself.
    ///
    /// `pub(crate)` because the rail names a shell the same way. A shell that
    /// reports no title of its own is named from its cwd here, and its rail
    /// row used to keep the literal "Terminal" it was created with - one
    /// surface reading "Terminal" in the rail and "splitlane" in its own header
    /// and in the status bar, in the same frame.
    pub(crate) fn cwd_label(cwd: &str) -> Option<String> {
        let trimmed = cwd.trim();
        if trimmed.is_empty() {
            return None;
        }
        let path = std::path::Path::new(trimmed);
        if dirs::home_dir().as_deref() == Some(path) {
            return Some("~".into());
        }
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| Some(trimmed.to_string()))
    }

    fn tab_hover_motion_id(tab_id: gpui::EntityId) -> SharedString {
        SharedString::from(format!("pane-tab-motion-{}", tab_id.as_u64()))
    }

    fn close_hover_motion_id(tab_id: gpui::EntityId) -> SharedString {
        SharedString::from(format!("pane-tab-close-motion-{}", tab_id.as_u64()))
    }

    fn hover_motion_snapshot(&self, id: &SharedString) -> (Rc<Cell<f32>>, f32, f32, u64) {
        self.tab_bar_hover_motion
            .get(id)
            .map(|motion| {
                (
                    motion.live_progress.clone(),
                    motion.from,
                    motion.target,
                    motion.epoch,
                )
            })
            .unwrap_or_else(|| (Rc::new(Cell::new(0.0)), 0.0, 0.0, 0))
    }

    fn set_tab_bar_hover_target(
        &mut self,
        id: &SharedString,
        live_progress: &Rc<Cell<f32>>,
        target: f32,
    ) -> bool {
        let motion = self
            .tab_bar_hover_motion
            .entry(id.clone())
            .or_insert_with(|| TabBarHoverMotion::new(live_progress.clone()));
        if motion.target == target {
            return false;
        }

        motion.from = motion.live_progress.get();
        motion.target = target;
        motion.epoch = motion.epoch.saturating_add(1);
        true
    }

    fn clear_tab_hover_motion(&mut self, tab_id: gpui::EntityId) {
        self.tab_bar_hover_motion
            .remove(&Self::tab_hover_motion_id(tab_id));
        self.tab_bar_hover_motion
            .remove(&Self::close_hover_motion_id(tab_id));
    }

    /// Shared shell for tab-bar icon buttons. The live progress cell lets a
    /// rapid enter/exit reverse from the currently painted value instead of
    /// snapping to an endpoint.
    #[allow(clippy::too_many_arguments)]
    fn tab_bar_button_shell(
        &self,
        id: SharedString,
        icon: AnyElement,
        size: f32,
        radius: Pixels,
        base_tint: Hsla,
        hover_tint: Option<Hsla>,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (live_progress, from, target, epoch) = self.hover_motion_snapshot(&id);

        let hover_id = id.clone();
        let hover_live_progress = live_progress.clone();
        let mouse_up_id = id.clone();
        let mouse_up_live_progress = live_progress.clone();
        let mouse_up_out_id = id.clone();
        let mouse_up_out_live_progress = live_progress.clone();
        let hover_background = crate::app::constants::sidebar_tab_hover_background();
        let active_background = crate::app::constants::sidebar_tab_active_background();
        let button = div()
            .id(id.clone())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(size))
            .h(px(size))
            .rounded(radius)
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                let target = if *hovered { 1.0 } else { 0.0 };
                if this.set_tab_bar_hover_target(&hover_id, &hover_live_progress, target) {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _window, cx| {
                    if this.set_tab_bar_hover_target(&mouse_up_id, &mouse_up_live_progress, 1.0) {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, _window, cx| {
                    if this.set_tab_bar_hover_target(
                        &mouse_up_out_id,
                        &mouse_up_out_live_progress,
                        0.0,
                    ) {
                        cx.notify();
                    }
                }),
            )
            .cursor_pointer()
            .on_click(move |e, w, cx| handler(e, w, cx))
            .active(move |style| style.bg(active_background).opacity(0.82));

        let visual = div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius)
            .text_color(base_tint)
            .child(icon);

        let distance = (target - from).abs();
        let visual = if epoch == 0 || distance <= f32::EPSILON {
            live_progress.set(target);
            let tint = hover_tint
                .map(|hover_tint| base_tint.blend(hover_tint.opacity(target)))
                .unwrap_or(base_tint);
            visual
                .bg(hover_background.opacity(target))
                .text_color(tint)
                .into_any_element()
        } else {
            let animation_id = SharedString::from(format!("pane-action-hover-{id}-{epoch}"));
            let duration = Duration::from_secs_f32(
                Duration::from_millis(TAB_BAR_HOVER_MS).as_secs_f32() * distance,
            );
            visual
                .with_animation(
                    animation_id,
                    Animation::new(duration).with_easing(ease_out_quint()),
                    move |visual, delta| {
                        let progress = (from + (target - from) * delta).clamp(0.0, 1.0);
                        live_progress.set(progress);
                        let tint = hover_tint
                            .map(|hover_tint| base_tint.blend(hover_tint.opacity(progress)))
                            .unwrap_or(base_tint);
                        visual
                            .bg(hover_background.opacity(progress))
                            .text_color(tint)
                    },
                )
                .into_any_element()
        };

        button.child(visual).into_any_element()
    }

    /// Fixed-size wrapper for the far-right tab-bar action cluster.
    fn action_button_shell(
        &self,
        id: SharedString,
        icon: AnyElement,
        base_tint: Hsla,
        hover_tint: Option<Hsla>,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.tab_bar_button_shell(
            id,
            icon,
            ACTION_BUTTON_SIZE,
            tok::radius::BADGE,
            base_tint,
            hover_tint,
            handler,
            cx,
        )
    }

    /// Close a tab at the given index. Emits `PaneEvent::Remove` if the pane becomes empty.
    pub fn close_tab_at(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        let selected_id = self.tabs.get(self.selected_idx).map(TabContent::entity_id);
        let removed_id = self.tabs[idx].entity_id();
        self.clear_tab_hover_motion(removed_id);
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            cx.emit(PaneEvent::Remove);
            return;
        }
        self.restore_selection_after_removal(idx, removed_id, selected_id);
        cx.emit(PaneEvent::TabsChanged);
        cx.notify();
    }

    pub fn index_for_tab_id(&self, tab_id: gpui::EntityId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.entity_id() == tab_id)
    }

    fn restore_selection_after_removal(
        &mut self,
        removed_idx: usize,
        removed_id: gpui::EntityId,
        selected_id: Option<gpui::EntityId>,
    ) {
        if self.tabs.is_empty() {
            self.selected_idx = 0;
            return;
        }
        match selected_id {
            Some(id) if id != removed_id => {
                if let Some(idx) = self.index_for_tab_id(id) {
                    self.selected_idx = idx;
                } else {
                    self.selected_idx = self.selected_idx.min(self.tabs.len() - 1);
                }
            }
            _ => {
                self.selected_idx = removed_idx.min(self.tabs.len() - 1);
            }
        }
    }

    /// Shared `on_drag_move` body for every drag that can split a pane:
    /// resolve the cursor (relative to the content `bounds`) to a split edge
    /// and, when it changes, seed the overlay glide and request a repaint. Both
    /// drag types drive the same blue preview, so the geometry lives here once.
    fn apply_drag_edge(
        &mut self,
        bounds: gpui::Bounds<Pixels>,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let w = bounds.size.width.as_f32();
        let h = bounds.size.height.as_f32();
        let x = (pos.x - bounds.left()).as_f32();
        let y = (pos.y - bounds.top()).as_f32();
        self.overlay_pane_size = bounds.size;
        let edge = compute_drop_edge(w, h, x, y, SPLIT_EDGE_BAND);
        if self.drag_split_direction != edge {
            let live = self.overlay_current.get();
            self.overlay_from = if live.2 > 0.0 && live.3 > 0.0 {
                live
            } else {
                split_rect(self.overlay_prev_dir, w, h)
            };
            self.overlay_prev_dir = self.drag_split_direction;
            self.drag_split_direction = edge;
            self.overlay_seq = self.overlay_seq.wrapping_add(1);
            cx.notify();
        }
    }

    /// Light the whole pane as the drop target, with no split edge.
    ///
    /// A rail surface dropped on a pane changes what that pane shows; it does
    /// not split it, so the edge bands are not read and the preview covers the
    /// slot. It still has to run, because the preview overlay carries the drop
    /// hitbox and that hitbox has no size until a drag-move has told the pane
    /// how big it is.
    fn apply_drag_whole_pane(&mut self, bounds: gpui::Bounds<Pixels>, cx: &mut Context<Self>) {
        self.overlay_pane_size = bounds.size;
        if self.drag_split_direction.is_some() {
            self.overlay_prev_dir = self.drag_split_direction;
            self.drag_split_direction = None;
            self.overlay_seq = self.overlay_seq.wrapping_add(1);
        }
        cx.notify();
    }

    /// Give this pane back to the launcher, keeping the pane itself.
    ///
    /// Not [`Self::close_selected_tab`], which emits `Remove` and takes the
    /// slot with it: this is the other half of a swap, where the surface went
    /// somewhere else and the slot stays open with nothing in it - the
    /// ladder's second rung, and a pane in every sense.
    pub fn clear_to_launcher(&mut self, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let displaced = std::mem::take(&mut self.tabs);
        self.selected_idx = 0;
        for gone in displaced {
            self.clear_tab_hover_motion(gone.entity_id());
        }
        cx.emit(PaneEvent::TabsChanged);
        cx.notify();
    }

    /// What this pane is showing, for a caller that means to put it somewhere
    /// else. `None` for the launcher, which has nothing to hand over.
    pub fn surface(&self) -> Option<TabContent> {
        self.tabs.get(self.selected_idx).cloned()
    }

    /// Human-readable label for this pane's active tab - what the rail row
    /// menu names a destination by, since panes carry no numbers anywhere a
    /// person can see them.
    pub fn active_tab_label(&self, cx: &App) -> String {
        self.tabs
            .get(self.selected_idx)
            .map(|t| Self::tab_title(t, cx))
            // A pane with no tabs is not "empty" to the person looking at it:
            // it is the launcher, and that is what its header and its rail row
            // should say.
            .unwrap_or_else(|| "New pane".into())
    }

    /// Close the currently selected tab. Returns `true` if the pane is now empty.
    pub fn close_selected_tab(&mut self, cx: &mut Context<Self>) -> bool {
        self.close_tab_at(self.selected_idx, cx);
        self.tabs.is_empty()
    }

    /// Get the currently selected terminal entity, if any. Returns `None`
    /// when the active tab is a markdown viewer or the pane is empty - all
    /// callers must handle the absence (event handlers, workspace ops, IPC,
    /// in-pane action buttons) so a markdown tab never triggers a panic.
    pub fn active_terminal_opt(&self) -> Option<&Entity<TerminalView>> {
        self.tabs
            .get(self.selected_idx)
            .and_then(TabContent::as_terminal)
    }

    // -----------------------------------------------------------------------
    // Tab bar rendering - Zed-style design
    // -----------------------------------------------------------------------

    /// Open the inline rename on `idx`, pre-filled and fully selected so the
    /// first keystroke replaces the old name.
    ///
    /// Enter commits, Escape cancels, and everything else - paste, selection,
    /// the arrows, IME - is the field's own, which is the whole point of
    /// building it out of the widget rather than out of an `on_key_down` arm.
    /// An empty field is a real answer here (it clears the custom name and
    /// hands the surface back to its derived one), so the field is told to
    /// submit on empty; without that, Enter on a cleared name is swallowed and
    /// the rename can only ever be cancelled.
    fn begin_rename(
        &mut self,
        idx: usize,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = cx.weak_entity();
        let input = cx.new(|cx| {
            let mut field = crate::widgets::text_area::TextArea::new("Name this surface", cx);
            field.set_value(current, cx);
            field.set_submit_on_empty(true);
            field.select_all_text(cx);
            let on_commit = pane.clone();
            field.on_submit(move |text, window, app| {
                let _ = on_commit
                    .clone()
                    .update(app, |this, cx| this.commit_rename(text, window, cx));
            });
            let on_cancel = pane;
            field.on_escape(move |window, app| {
                let _ = on_cancel
                    .clone()
                    .update(app, |this, cx| this.cancel_rename(window, cx));
            });
            field
        });
        let focus = input.read(cx).focus_handle.clone();
        self.rename = Some(TabRename { idx, input });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Render a tab's title slot. While that tab is being renamed the
    /// slot becomes a focusable inline input capturing keystrokes; otherwise
    /// it's the normal ellipsized title.
    fn render_tab_title(&self, i: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = tab_colors();
        if let Some(state) = self.rename.as_ref().filter(|r| r.idx == i) {
            div()
                .flex_1()
                .min_w_0()
                .bg(ui.overlay)
                .px_1()
                .rounded(tok::radius::METER)
                .child(state.input.clone())
                .into_any_element()
        } else {
            let full_title = Self::tab_full_title(&self.tabs[i], cx);
            let display_title = Self::tab_title(&self.tabs[i], cx);
            let show_tooltip = full_title != display_title
                || full_title.chars().count() > TAB_TITLE_TOOLTIP_THRESHOLD;
            let mut title = div()
                .id(SharedString::from(format!("pane-tab-title-{i}")))
                .flex_1()
                .min_w_0()
                .overflow_x_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(tok::text::ROW)
                .child(display_title);
            if show_tooltip && !self.menu_open {
                title = title.tooltip(crate::ui_primitives::text_tooltip(full_title));
            }
            title.into_any_element()
        }
    }

    /// Commit the in-progress inline rename: a non-empty buffer sets
    /// the tab's terminal custom name; an empty one clears it (reverting to the
    /// auto-derived name). Emits `SurfaceRenamed` so the app persists the
    /// session, then returns focus to the terminal.
    ///
    /// Takes the text rather than reading it back off the field: the callback
    /// fires from *inside* the `TextArea`'s own update, so re-reading the
    /// entity there panics. The same reason `apply_agents_rename` takes one.
    fn commit_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.rename.take() else {
            return;
        };
        if let Some(TabContent::Terminal(t)) = self.tabs.get(state.idx).cloned() {
            let trimmed = name.trim();
            let new_name = (!trimmed.is_empty()).then(|| trimmed.to_string());
            let thread_id = t.read(cx).agent_thread_id;
            t.update(cx, |view, _cx| {
                view.terminal.custom_name = new_name.clone();
            });
            // Read after the write, so a cleared name reports what the header
            // has fallen back to rather than what it used to say.
            let display = Self::tab_title(&TabContent::Terminal(t), cx);
            // The rail names the same surface from its record, not from this
            // field, so a header-only write left one surface under two names
            // depending on where it was read. The app writes the record.
            cx.emit(PaneEvent::SurfaceRenamed {
                thread_id,
                name: new_name,
                display,
            });
        }
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Drop the rename without applying it, and give the surface its focus
    /// back - `Escape` on the field.
    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.take().is_none() {
            return;
        }
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn render_tab_bar(
        &self,
        is_focused: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab_count = self.tabs.len();
        let ui = tab_colors();
        // Handle to this pane, captured once for the per-tab drag closures:
        // same-pane vs cross-pane is decided by comparing the
        // drag's `source_pane` to this entity. `accent` tints the insertion
        // indicator drawn during a same-pane reorder hover.
        // The strip used to take the terminal background so it melted into the
        // body below it - one clean surface. The design draws the header as a
        // band of its own instead, and gives it the job of saying whether this
        // slot is the one taking input.
        let bar_bg = tab_bar_background(
            &ui,
            self.cached_config.windows_terminal_material_enabled(),
            is_focused,
        );

        // Outer container: full-width, fixed height, tab_bar background. The
        // chips are shorter than the bar, so center them vertically to float.
        //
        // The canvas measures it: nothing in `Pane` knows how wide the header
        // is - that is the layout's answer - and the action cluster needs it
        // to decide whether it fits beside the surface's name. Zero-size and
        // absolute, so it takes part in no flex arithmetic of its own.
        let measured = self.header_width.clone();
        let bar = div()
            .relative()
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .w_full()
            .h(TAB_BAR_HEIGHT)
            .bg(bar_bg)
            // The header's fill is the slot's top edge, so it carries the
            // slot's radius. Without it the fill's square corner sits inside
            // the arc the border draws and shows as a notch - invisible while
            // the header was the terminal's own colour, plain once it became a
            // band of its own.
            .rounded_t(tok::radius::PANEL)
            .child(div().absolute().inset_0().child(gpui::canvas(
                move |bounds, _window, _cx| {
                    measured.set(f32::from(bounds.size.width));
                },
                |_, _, _, _| {},
            )));

        // Scrollable tab area (Zed pattern: overflow_x_scroll on inner row)
        let tabs_area = div()
            .id("pane-tabs-area")
            .relative()
            .flex_1()
            .h_full()
            .overflow_x_hidden()
            // No double-click-to-add here either. It was the `+` button's
            // hidden twin - same effect, no affordance - and it went with it in
            // a redesign: a surface is created from the rail or by a chord,
            // and a third door in the header puts creation in the row that says
            // what the pane *is*.
            ;

        let mut tabs_row = div()
            .id("pane-tabs-scroll")
            .flex()
            .flex_row()
            .items_center()
            .h_full()
            .gap(px(STRIP_GAP))
            .pl(px(crate::app::constants::PANE_CONTENT_INSET))
            .overflow_x_scroll();

        // A slot showing one surface draws no tab strip - it says what
        // it is showing. A single chip would offer a choice between one thing
        // and put a close box on the only surface in the slot; the name is
        // what the agent surface's header has always shown, and this is that
        // same header. The `+` stays either way: creating is not a choice
        // between existing surfaces.
        if tab_count <= 1 {
            tabs_row = tabs_row
                .child(self.render_single_surface_name(ui, cx))
                // The design's order, from its own markup: name, badge,
                // branch, rerun, status. They sit beside the name rather than
                // at the far end of the header, because every one of them is a
                // fact about the surface the name belongs to.
                .children(self.render_surface_badge(ui, cx))
                .children(self.render_surface_branch(ui))
                .children(self.render_surface_rerun(ui, cx))
                // Stop takes the status word's slot rather than adding one, so
                // this is a choice of one and not a pair. The ladder keeps it on
                // every rung of the ladder, including the narrowest.
                .children(match self.render_surface_stop(ui, cx) {
                    Some(stop) => Some(stop),
                    None => self.render_surface_status(ui),
                });
        }
        let tabs_area = tabs_area.child(tabs_row);

        bar.child(tabs_area).child(self.render_end_section(cx))
    }

    /// The left side of a slot header showing one surface: its glyph and name,
    /// through the same helper the app's own slot header uses (one slot
    /// header, one idiom, whoever is drawing it).
    ///
    /// Double-click renames, exactly as it does on a tab chip, so the one
    /// affordance a chip carries that a name does not is not actually lost.
    fn render_single_surface_name(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // A rename in progress takes over the whole left side: the input is
        // the tab's own, so committing and cancelling behave the same here.
        if self.rename.as_ref().map(|r| r.idx) == Some(self.selected_idx) {
            return div()
                .flex_none()
                .w(px(TAB_WIDTH))
                .min_w_0()
                .pl(tok::space::XS)
                .child(self.render_tab_title(self.selected_idx, cx))
                .into_any_element();
        }
        let Some(tab) = self.tabs.get(self.selected_idx) else {
            // A pane with no session in it: the header names what it is
            // showing, which is the launcher.
            return div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::SM)
                .pl(tok::space::XS)
                .child(
                    div()
                        .flex_none()
                        .w(px(12.))
                        .text_size(tok::text::ROW)
                        .text_color(ui.muted)
                        .child("+"),
                )
                .child(
                    div()
                        .text_size(tok::text::ROW)
                        .text_color(ui.text_secondary)
                        .child("New pane"),
                )
                // The badge says what a header for a pane with nothing in it
                // can honestly say. It carries no status word, no context and
                // no `\u{22ef}` menu either - there is nothing yet to report on
                // or to act on.
                .child(
                    div()
                        .flex_none()
                        .items_center()
                        .px(tok::space::SM)
                        .py(px(2.))
                        .ml(tok::space::SM)
                        .rounded(tok::radius::BADGE)
                        .border_1()
                        .border_color(ui.border)
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::HINT)
                        .text_color(ui.muted)
                        .child("nothing in it yet"),
                )
                .into_any_element();
        };
        // `dim`, the one neutral step every type glyph in the app takes - the
        // rail's, the launcher's and this one. It was `muted`,
        // one step brighter, which said nothing by being different.
        let glyph = match Self::tab_glyph(tab, cx) {
            Some(mark) => div()
                .flex_none()
                .w(px(12.))
                .text_size(tok::text::ROW)
                .text_color(ui.dim)
                .child(mark)
                .into_any_element(),
            None => svg()
                .size(px(12.))
                .flex_none()
                .path(Self::tab_icon(tab))
                .text_color(ui.dim)
                .into_any_element(),
        };
        let title = SharedString::from(Self::tab_title(tab, cx));
        div()
            .id("pane-slot-name")
            .flex_none()
            .min_w_0()
            .flex()
            .cursor_pointer()
            .on_click(cx.listener(|this, e: &ClickEvent, window, cx| {
                if matches!(e, ClickEvent::Mouse(m) if m.down.click_count == 2) {
                    let idx = this.selected_idx;
                    if let Some(TabContent::Terminal(t)) = this.tabs.get(idx) {
                        let current = t.read(cx).terminal.custom_name.clone().unwrap_or_default();
                        this.begin_rename(idx, current, window, cx);
                    }
                }
                cx.stop_propagation();
            }))
            // A shell has no turn to be in and takes no dot. An agent surface
            // living in this slot does: this is where the attention signal
            // that used to glow around the whole pane now lives, so a waiting
            // agent in an unfocused slot is still visible from across the
            // window.
            .child(crate::app::slot_header::slot_header_name(
                glyph,
                title,
                self.single_surface_status(),
                ui,
            ))
            .into_any_element()
    }

    /// The four wedges that make the slot's corners round.
    ///
    /// GPUI rounds a quad, not the children painted inside it: the header
    /// paints its own fill and the terminal paints a full-bounds background of
    /// its own, both square, so a radius on this container alone would round
    /// the border and nothing behind it. The wedges paint the *outside* of
    /// each arc in the colour of the panes area the slot sits on - the same
    /// trick the main panel uses for its own corners - which rounds the slot
    /// whatever surface is inside it.
    ///
    /// Skipped under Windows terminal material: there the panes area is the
    /// native backdrop rather than a colour this can name, and painting the
    /// gutter over it would be a patch. The slot keeps square corners on that
    /// path.
    fn render_corner_masks(&self) -> Vec<gpui::AnyElement> {
        use crate::{PanelCorner, panel_corner_mask};

        if self.cached_config.windows_terminal_material_enabled() && cfg!(target_os = "windows") {
            return Vec::new();
        }
        let gutter = tab_colors().gutter;
        // Flush with the pane's box, and `PANEL`-sized: the wedge, the pane and
        // the focus ring then share one centre, and the wedge reaches the pane's
        // own edge.
        //
        // It used to sit at a 1px inset, to keep it off "the 1px ring that says
        // where focus is". That was unnecessary - the ring is added to this
        // element **after** the masks, so it paints over them either way - and
        // it cost the thing it was protecting. The inset moved the wedge's arc
        // centre a pixel down and right of the pane's, so the two crossed, and
        // the pane's own white background showed **outside** the ring at each
        // corner. Mapped on Harbor Light, 27 August 2026:
        //
        // ```text
        //   ....WGG.c      W outside the ring - the white frame that was reported
        //   ....WWGGg
        //   ........GGGG
        // ```
        //
        // GPUI does not clip a child to its parent's radius, so that white
        // square corner is the terminal's own fill; only the wedge takes it
        // away, and it can only do that where it actually reaches.
        let inset = px(0.);
        let size = tok::radius::PANEL;
        [
            (PanelCorner::TopLeft, true, true),
            (PanelCorner::TopRight, false, true),
            (PanelCorner::BottomLeft, true, false),
            (PanelCorner::BottomRight, false, false),
        ]
        .into_iter()
        .map(|(corner, left, top)| {
            div()
                .absolute()
                .when(left, |d| d.left(inset))
                .when(!left, |d| d.right(inset))
                .when(top, |d| d.top(inset))
                .when(!top, |d| d.bottom(inset))
                .size(size)
                .child(panel_corner_mask(corner, gutter))
                .into_any_element()
        })
        .collect()
    }

    /// The status the slot header reports for the surface it is showing, or
    /// `None` for a surface with no turn to be in.
    ///
    /// Errored wins, exactly as it does on a tab chip: a crash must not hide
    /// behind a waiting dot. Below that the surface record's own status is what
    /// answers - the same field the rail's row reads, and the only one of the
    /// three sources that knows about `Thinking` - and the `attention` map is
    /// the fallback for a surface the app pushed no facts for.
    ///
    /// One resolved status, read by both signals in this header: the dot and
    /// the word. They cannot disagree, which is the whole reason this is a
    /// function and not two lookups.
    fn single_surface_status(&self) -> Option<crate::project::ThreadStatus> {
        let id = self.tabs.get(self.selected_idx)?.as_terminal()?.entity_id();
        if self.errored.contains(&id) {
            return Some(crate::project::ThreadStatus::Failed);
        }
        if let Some(status) = self.surface_facts.get(&id).and_then(|facts| facts.status) {
            return Some(status);
        }
        if self.attention.contains_key(&id) {
            Some(crate::project::ThreadStatus::WaitingForInput)
        } else {
            None
        }
    }

    /// "↻ rerun" for a shell pane, naming the command it will run.
    ///
    /// Shells only, which is the design's own rule: an agent's turn is not a
    /// command and has no equivalent. Level 2 of the ladder - it outlives the
    /// badge and the branch, because on a narrow shell pane it is the only
    /// thing in the header that DOES anything.
    ///
    /// Absent when the terminal cannot name the last command: no OSC 133 shell
    /// integration, a command still running, or a report the shell could not
    /// carry faithfully. A button that offers to run something it cannot name
    /// is a worse affordance than no button.
    ///
    /// A click submits only what the header showed in full. When the label
    /// elides, the click types the command at the prompt instead and leaves
    /// the Enter to the user - the elision is exactly where a second command
    /// would hide, and untrusted output can print an OSC 133 as easily as a
    /// shell can.
    fn render_surface_rerun(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.tabs.len() != 1 || self.header_level() < HEADER_LEVEL_RERUN {
            return None;
        }
        if self.active_surface_facts()?.is_agent {
            return None;
        }
        // The terminal is captured, not re-resolved on click: the surface that
        // reported the command is the only one allowed to be sent it, and the
        // selection can move between the frame that drew this and the click
        // that lands on it.
        let terminal = self.tabs.get(self.selected_idx)?.as_terminal()?.clone();
        let command = terminal.read(cx).last_command()?;
        let (shown, whole) = crate::app::pane_header::rerun_command_label(&command);
        let label = SharedString::from(format!("\u{21bb} rerun: {shown}"));
        // A label the user can read in full is one they can consent to
        // running. An elided one is not - the elision is exactly where a
        // second command would hide - so that click types the command at the
        // prompt and leaves the Enter to them.
        let tooltip = if whole {
            format!("Run again: {command}")
        } else {
            format!("Type it at the prompt - press \u{23ce} to run: {command}")
        };
        let to_send = command.clone();
        Some(
            div()
                .id("pane-btn-rerun")
                .flex_none()
                .ml(tok::space::SM)
                .whitespace_nowrap()
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                .text_color(ui.muted)
                .cursor_pointer()
                .hover(|style| style.text_color(ui.text))
                .when(!self.menu_open, |chip| {
                    chip.tooltip(crate::ui_primitives::text_tooltip(tooltip))
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |_this, _e: &ClickEvent, _window, cx| {
                    let terminal = terminal.read(cx);
                    if whole {
                        terminal.send_command(&to_send);
                    } else {
                        terminal.type_command(&to_send);
                    }
                    cx.stop_propagation();
                }))
                .child(label)
                .into_any_element(),
        )
    }

    /// The branch beside the badge, and `⑂ branch` in the warning role
    /// when the checkout is a linked git worktree.
    ///
    /// The mark and the colour are one signal, not decoration: two panes on the
    /// same repository showing different branches is the ordinary case here,
    /// and a worktree is the one where the files under the pane are a second
    /// checkout rather than the one the rest of the window is looking at.
    ///
    /// Level 3, with the badge. A rendered document and a diff take none: the
    /// document is a file rather than a working tree, and the diff names its
    /// own revisions in its own header.
    fn render_surface_branch(&self, ui: crate::theme::UiColors) -> Option<gpui::AnyElement> {
        if self.tabs.len() != 1 || self.header_level() < HEADER_LEVEL_BADGE {
            return None;
        }
        if !matches!(self.tabs.get(self.selected_idx)?, TabContent::Terminal(_)) {
            return None;
        }
        let (branch, is_worktree) = self.active_surface_facts()?.branch.clone()?;
        let (label, color) = if is_worktree {
            (
                SharedString::from(format!("\u{2442} {branch}")),
                ui.vc_modified,
            )
        } else {
            (branch, ui.muted)
        };
        Some(
            div()
                .flex_none()
                .ml(tok::space::SM)
                .whitespace_nowrap()
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                .text_color(color)
                .child(label)
                .into_any_element(),
        )
    }

    /// The status word beside the name (the design: "status text, coloured
    /// by status"). Level 1 of the priority ladder: after the badge and the
    /// branch have gone, and after the meter, this is the last thing standing
    /// between the name and the `⋯`.
    ///
    /// Agent surfaces only, which is the design's own rule: a shell has no
    /// turn to be in, and the word for one would always be the same word.
    fn render_surface_status(&self, ui: crate::theme::UiColors) -> Option<gpui::AnyElement> {
        if self.tabs.len() != 1 || self.header_level() < HEADER_LEVEL_STATUS {
            return None;
        }
        if !self.active_surface_facts()?.is_agent {
            return None;
        }
        let (word, color) =
            crate::app::slot_header::slot_header_status_word(self.single_surface_status()?, ui);
        Some(
            div()
                .flex_none()
                .ml(tok::space::SM)
                .whitespace_nowrap()
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                .text_color(color)
                .child(word)
                .into_any_element(),
        )
    }

    /// The Stop button, and the whole of its rule.
    ///
    /// **It exists only while a run is going.** Not "always there, greyed out
    /// when idle": a dimmed stop button is a promise that cannot be kept, and
    /// it holds space the header is fighting over. **It takes the status word's
    /// slot** rather than adding one - while the run is going, "running" and a
    /// Stop button say the same thing, and only one of them can be pressed.
    /// **It is never dropped**, at any width: it is the one control that undoes
    /// what the pane is doing. Below 420px it shrinks to the bare glyph.
    ///
    /// It presses ESC into the surface's own PTY, which is how every one of
    /// these CLIs is interrupted. The double-ESC hazard - a second ESC opens
    /// message editing in Claude Code - is answered by the button's own
    /// existence condition rather than by a second guard: the button is on
    /// screen only while the agent is generating, which is exactly when the
    /// first ESC is the interrupt.
    fn render_surface_stop(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.tabs.len() != 1 || !self.active_surface_facts()?.is_agent {
            return None;
        }
        if self.single_surface_status()? != crate::project::ThreadStatus::Thinking {
            return None;
        }
        // Captured, not re-resolved on click: the surface that was running when
        // this was drawn is the only one allowed to be interrupted by it.
        let terminal = self.tabs.get(self.selected_idx)?.as_terminal()?.clone();
        let wide = self.header_level() >= HEADER_LEVEL_RERUN;
        let mut button = div()
            .id("pane-btn-stop")
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .ml(tok::space::SM)
            .px(tok::space::SM)
            .rounded(tok::radius::BADGE)
            .whitespace_nowrap()
            .font_family(tok::font::MONO)
            .text_size(tok::mono::HINT)
            .text_color(ui.vc_deleted)
            .cursor_pointer()
            .hover(|style| style.bg(with_alpha(ui.vc_deleted, 0.12)))
            .when(!self.menu_open, |b| {
                b.tooltip(crate::ui_primitives::text_tooltip(
                    "Stop the run (ESC into the agent's terminal)",
                ))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |_this, _e: &ClickEvent, _window, cx| {
                terminal.read(cx).send_text("\u{1b}");
                cx.stop_propagation();
            }))
            // Square, unrounded. The smallest radius token on a 7px glyph draws
            // a disc, and a red disc beside the status dot reads as a second
            // status dot - `failed` - which is precisely what the bare glyph
            // below 420px is not. The shape is what says "stop".
            .child(div().flex_none().size(STOP_GLYPH).bg(ui.vc_deleted));
        if wide {
            button = button.child("Stop");
        }
        Some(button.into_any_element())
    }

    /// The design's pane-header priority ladder (which replaces the
    /// earlier binary "narrow" rule): the header drops whole items by measured
    /// width instead of shrinking everything, so what is left always reads.
    ///
    /// | width | header shows |
    /// |---|---|
    /// | ≥ 520 | name, badge, branch, rerun, status or Stop, ⋯, × |
    /// | 420-519 | name, rerun, status or Stop, ⋯, × |
    /// | 320-419 | name, status or Stop (bare ■), ⋯, × |
    /// | < 320 | name, Stop (bare ■), ⋯, × |
    ///
    /// The close ×, the surface's ⋯ and **Stop** are never dropped, and the
    /// name keeps its 34px floor - a header that has stopped saying what the
    /// pane is showing has stopped doing its job. Stop and the status word
    /// share one slot rather than taking two (`render_surface_stop`), which is
    /// why the ladder reads "status or Stop" and not both.
    ///
    /// Before the header's action cluster was taken out this was an arithmetic
    /// budget (`header_is_narrow`) that counted the buttons and gave the name
    /// what was left. With the buttons gone there is nothing to budget for, and
    /// the design's own measured ladder is what belongs here.
    fn header_level(&self) -> u8 {
        let width = self.header_width.get();
        // Before the first layout nothing has been measured, and an unmeasured
        // header shows the least it can. Assuming the widest step instead put
        // a full row of facts into a header whose width was still unknown: on
        // a narrow pane that row overflowed for one frame and pushed the
        // trailing `⋯` and `×` outside the clip, which is the one
        // thing the design says may never happen. Arriving on the second frame
        // is the cheaper mistake.
        if width <= 0. {
            return 0;
        }
        if width >= 520. {
            HEADER_LEVEL_BADGE
        } else if width >= 420. {
            HEADER_LEVEL_RERUN
        } else if width >= 320. {
            HEADER_LEVEL_STATUS
        } else {
            0
        }
    }

    /// The right end of the slot header: the zoom mark, the surface's own
    /// menu, then the close ×.
    ///
    /// It used to be a cluster of eight unlabelled icons left over from the
    /// old cockpit - copy-ref, two splits, Files, session history, the custom
    /// commands, a fold chevron and the transcript/terminal chip - none of
    /// which the design puts in a header at all (a header lists information,
    /// not actions). Splits and Files were promises the toolbar and the palette
    /// already keep; the other four moved into the tab menu the `⋯` opens,
    /// which is where an action over a surface lives: where its object lives. The chevron went
    /// with what it folded.
    ///
    /// Order is the design's: `⋯` then `×`, with the × always last and
    /// always inside. The old cluster had them the other way round.
    fn render_end_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = tab_colors();
        let mut end_section = div()
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .h_full()
            .px(px(SECTION_PX))
            .gap(px(0.));

        // The context meter, which the design puts between the spacer and the
        // `⋯` - the last of the header's facts before its controls begin.
        //
        // Two phases, and which one is drawn is decided by **what is known**
        // rather than by how wide the pane is. With a ceiling there is a share,
        // and the arc is eleven pixels of no text at all, so it appears at
        // every width including the narrowest pane the app will make. Without
        // one there is only a count, a count is text, and text goes by the
        // ladder like every other fact in this header.
        if let Some(context) = self.active_surface_facts().and_then(|f| f.context) {
            let level = self.header_level();
            let tooltip = context.tooltip_lines();
            let mut meter = div()
                .flex_none()
                .flex()
                .items_center()
                .gap(tok::space::XS)
                .px(tok::space::XS);
            if let Some(fill) = context.fraction() {
                meter = meter.child(context_arc(fill, context.just_compacted(), ui));
            }
            // The text goes by the ladder like every other fact in this header;
            // the arc does not, because it is not text. At the widest rung both
            // numbers, in one unit, so a reader compares rather than converts.
            if level >= HEADER_LEVEL_RERUN {
                meter = meter.child(
                    div()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::HINT)
                        .text_color(ui.dim)
                        .child(SharedString::from(
                            context.header_label(level >= HEADER_LEVEL_BADGE),
                        )),
                );
            }
            if context.fraction().is_some() || level >= HEADER_LEVEL_RERUN {
                end_section = end_section.child(
                    meter
                        .id("context-meter")
                        .tooltip(crate::ui_primitives::lines_tooltip(tooltip)),
                );
            }
        }

        // The zoom mark is information - "this pane is filling the window" -
        // and stays for the same reason the badge and the status word do. The
        // design has no zoom, so it has nothing to say about where it sits.
        if self.zoomed {
            end_section = end_section.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(tok::space::XS)
                    .mr(tok::space::XS)
                    .h(px(18.))
                    .rounded(tok::radius::METER)
                    .bg(ui.accent)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.base)
                    .child("Z"),
            );
        }

        // The active surface's own menu, at the right end of the slot header,
        // because an action lives where its object lives. It opens the menu a
        // right-click on the tab opens; a slot showing one surface has no chip
        // to right-click, so without this the menu had no home at all.
        // A pane holding nothing has no menu: every entry in it is an action
        // over a surface, and the button drew itself and then returned on
        // click - a control that refuses every press.
        if !self.tabs.is_empty() {
            end_section = end_section.child(crate::app::slot_header::slot_overflow_button(
                SharedString::from("pane-slot-overflow"),
                ui,
                self.menu_open,
                cx.listener(|this, e: &ClickEvent, _window, cx| {
                    let Some(tab) = this.tabs.get(this.selected_idx) else {
                        return;
                    };
                    if let Some(position) = e.mouse_position() {
                        cx.emit(PaneEvent::OpenTabMenu {
                            tab_id: tab.entity_id(),
                            position,
                        });
                    }
                    cx.stop_propagation();
                }),
            ));
        }

        // The design's close ×: always the last thing in the header, and
        // always there.
        //
        // It used to be hidden on the only pane, because "there is no 'close
        // this one' when there is only one, and the row would be offering to
        // empty the screen". Emptying the screen is now a state the app has -
        // closing the last pane leaves the container with none and says where
        // the next one comes from - so the × is offering something real. While
        // it was hidden, a pane the app itself had opened and the user did not
        // want (the launcher after `+ agent`) could not be dismissed at all
        // once its neighbour was gone: no ×, and nothing in a launcher for the
        // rail row's delete to act on.
        end_section = end_section.child(
            self.action_button_shell(
                SharedString::from("pane-btn-close-slot"),
                svg()
                    .size(px(12.))
                    .flex_none()
                    .path("icons/close.svg")
                    .text_color(ui.muted)
                    .into_any_element(),
                ui.muted,
                Some(ui.text),
                cx.listener(|_this, _e: &ClickEvent, _window, cx| {
                    cx.emit(PaneEvent::CloseSlot);
                    cx.stop_propagation();
                }),
                cx,
            ),
        );

        end_section
    }
}

/// The launcher's filter field, one per pane.
///
/// Free-standing because both constructors build one and neither has a `Pane`
/// to call a method on yet.
fn new_launcher_query(cx: &mut Context<Pane>) -> Entity<crate::widgets::text_input::TextInput> {
    let input = cx.new(|cx| {
        crate::widgets::text_input::TextInput::new(
            "",
            "Filter sessions, or start something new",
            cx,
        )
    });
    // Every keystroke re-filters, and the pane is what draws the result.
    cx.observe(&input, |_, _, cx| cx.notify()).detach();
    input
}

/// The launcher: what can go into a pane that holds nothing yet.
///
/// One filtered list in three parts - start something new, open a session that
/// is in no pane, resume one from disk - drawn inside the pane it will fill.
/// The rows come from `SplitlaneApp::sync_launcher_rows`; see `app/launcher.rs`
/// for why the pane builds none of them.
///
/// This replaced a grid of agent tiles. The grid answered "which
/// CLI", which is a narrower question than the one being asked: to put an
/// already-running session in this pane you had to leave it and go to the rail.
///
/// **The list is reachable from the keyboard, which the design does not
/// specify**. `⌥\` is a keystroke; a pane it opens
/// that can only be finished with a mouse gives back what the keystroke was
/// for. So `↑` / `↓` / `enter` behave exactly as they do in `⌘K`, and the
/// selection is provisional until then - it starts on the first row and every
/// edit of the query puts it back there.
fn render_pane_launcher(pane: &Pane, cx: &mut Context<Pane>) -> gpui::AnyElement {
    use crate::app::launcher::LauncherGroup;
    let rows = pane.visible_launcher_rows(cx);
    let selected = pane.launcher_selected.min(rows.len().saturating_sub(1));
    let query_text = pane.launcher_query.read(cx).value().to_string();
    let on_empty_pane = pane.tabs.is_empty();
    let anchor = &pane.launcher_anchor;
    let scroll = &pane.launcher_scroll;
    let query = &pane.launcher_query;
    let ui = tab_colors();
    let hover_bg = ui.subtle;

    // The rows sit directly in the scrolling element, because `scroll_to_item`
    // indexes that element's own children - a wrapper around them would make
    // every index 0. Group headings are children too, so the row's position in
    // the list is not its child index and has to be recorded as it is drawn.
    //
    // For the same reason every child carries `flex_none` itself. A scroller's
    // items have no automatic minimum height and shrink by default, so a row's
    // `h(tok::row::SEARCH)` was only a starting point: when the history arrived
    // and the list outgrew the pane, every row gave up height to fit rather
    // than the list overflowing and scrolling.
    let mut list = div()
        .id("pane-launcher-list")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .p(tok::space::MD)
        .overflow_y_scroll()
        .track_scroll(scroll);
    let mut child_index = 0usize;
    let mut selected_child: Option<usize> = None;
    let mut drawn_group: Option<LauncherGroup> = None;
    for (idx, row) in rows.iter().enumerate() {
        // A group with no rows is not there at all, rather than a heading over
        // nothing: the third part arrives late and an empty "RECENT ON DISK"
        // would read as "you have no history" until it did.
        if drawn_group != Some(row.group) {
            drawn_group = Some(row.group);
            list = list.child(
                // The app's one section label, not a second one built here.
                // This was `mono::HINT` - the token for "the smallest thing
                // drawn: a counter, a badge" - while `PROJECTS`, `FILES` and
                // `UNCOMMITTED` are all `mono::LABEL`. Half a pixel, and the
                // wrong half: a size chosen by nearest number rather than by
                // role, which is the failure `ui_tokens` is written to prevent.
                //
                // The padding stays asymmetric (more above than below) because
                // it is doing a job the rail's single heading does not: it
                // separates two lists, and the heading has to bind to the rows
                // under it rather than float between them.
                crate::ui_primitives::section_eyebrow(SharedString::from(row.group.label()), ui)
                    .flex_none()
                    .pt(tok::space::LG)
                    .pb(tok::space::XS)
                    .px(tok::space::LG),
            );
            child_index += 1;
        }
        let is_selected = idx == selected;
        if is_selected {
            selected_child = Some(child_index);
        }
        child_index += 1;
        let action = row.action.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("pane-launcher-row-{idx}")))
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::MD)
                .w_full()
                .flex_none()
                .h(tok::row::SEARCH)
                .px(tok::space::LG)
                .rounded(tok::radius::SMALL)
                .relative()
                // `list_selection`, one step off hover, plus the design's
                // leading 2px bar - GPUI has no inset shadow, so it is a real
                // child. Both halves are needed: while the highlight was
                // `subtle` it was exactly what hover paints, so the row the
                // arrows were on and a row under the pointer read the same.
                .when(is_selected, |r| {
                    r.bg(ui.list_selection).child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(px(2.))
                            .rounded_l(tok::radius::SMALL)
                            .bg(ui.accent),
                    )
                })
                .when(!is_selected, |r| r.hover(|r| r.bg(hover_bg)))
                // The row is a click target inside a focused field; without
                // this the mouse-down moves focus out of the query first and
                // the click lands on a pane that has already redrawn.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .cursor_pointer()
                .on_click(cx.listener(move |_this, _: &ClickEvent, _w, cx| {
                    cx.emit(PaneEvent::LauncherPick(action.clone()));
                    cx.stop_propagation();
                }))
                .child(
                    div()
                        .flex_none()
                        .w(px(11.))
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::ROW)
                        // One neutral step for every type glyph:
                        // the accent in a row means one thing, and only the
                        // trailing status word says it. `row.glyph_accent` was
                        // the flag that painted an agent's `◆` accent here, and
                        // it went with the colour rather than staying as a
                        // field that is always false.
                        .text_color(ui.dim)
                        .child(row.glyph.clone()),
                )
                // The name and, beside it, the directory it belongs to - one
                // group that takes the row's free space, so what is trimmed
                // when the row is too narrow is the name and nothing else.
                //
                // It used to be two children of the row plus a `flex_1`
                // spacer, and the name carried `flex_shrink(1.)`, which is
                // GPUI's default and therefore says nothing: the name kept a
                // **content** basis with no grow, sitting in a row where the
                // spacer's basis was zero and its grow was one. Every pixel of
                // free space went to the spacer and the name was measured
                // against what was left, so the wider the pane the less name
                // there was - a full-width stacked pane drew every row as a
                // single ellipsis. The `⌘K` palette had the right shape all
                // along: the label grows, and there is no spacer to compete
                // with it.
                // The name takes the row's free space, and it is `flex_1` -
                // grow, shrink, **basis zero** - so its width is what the flex
                // algorithm hands it rather than what the text measured. That
                // distinction is the whole bug: the name used to keep a
                // content basis and sit beside a `flex_1` spacer that took
                // every free pixel, and inside this scrolling column the
                // content basis settled at a width the pane no longer had -
                // about 125px whatever the row said. A pane split side by side
                // was narrow enough for that to pass unseen; stacked, at the
                // full width of the window, every row in the list drew as a
                // single ellipsis. The command palette had the right shape all
                // along: the label grows, and there is no spacer to compete
                // with it.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_x_hidden()
                        .text_ellipsis()
                        .text_size(tok::text::ROW)
                        .text_color(ui.text)
                        .child(row.label.clone()),
                )
                .when(!row.detail.is_empty(), |r| {
                    r.child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::HINT)
                            .text_color(ui.text_tertiary)
                            .child(row.detail.clone()),
                    )
                })
                .when(!row.meta.is_empty(), |r| {
                    r.child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::HINT)
                            .text_color(if row.meta_accent { ui.accent } else { ui.muted })
                            .child(row.meta.clone()),
                    )
                })
                .when(!row.keys.is_empty(), |r| {
                    r.child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::HINT)
                            .text_color(ui.dim)
                            .child(row.keys.clone()),
                    )
                }),
        );
    }
    if rows.is_empty() {
        let message = if query_text.trim().is_empty() {
            // Nothing to start and nothing to resume: every agent is off PATH
            // and this project has no sessions. Say which, because "no rows"
            // on a surface whose whole job is to offer some reads as broken.
            SharedString::from(
                "No agent CLI was found on PATH, and this project has no sessions yet.",
            )
        } else {
            SharedString::from(format!(
                "Nothing matches \u{201c}{}\u{201d}.",
                query_text.trim()
            ))
        };
        list = list.child(
            div()
                .flex_none()
                .p(tok::space::LG)
                .text_size(tok::text::ROW)
                .text_color(ui.text_tertiary)
                .child(message),
        );
    }

    list = list.child(
        div()
            .flex_none()
            .px(tok::space::LG)
            .pt(tok::space::LG)
            .pb(tok::space::XS)
            .font_family(tok::font::MONO)
            .text_size(tok::mono::HINT)
            .text_color(ui.faint)
            // What `Esc` does depends on what is underneath, so the line says
            // the true one rather than both.
            .child(SharedString::from(if on_empty_pane {
                "esc closes this pane \u{b7} drag a session in from the sidebar"
            } else {
                "esc gives the pane back \u{b7} drag a session in from the sidebar"
            })),
    );

    // Only when the selection has actually moved - see `launcher_scrolled_to`.
    if let Some(child) = selected_child
        && pane.launcher_scrolled_to.get() != Some(child)
    {
        pane.launcher_scrolled_to.set(Some(child));
        scroll.scroll_to_item(child);
    }

    div()
        .size_full()
        .flex()
        .flex_col()
        .min_h_0()
        .track_focus(anchor)
        // Every key the launcher answers, read before the field sees them.
        // `TextInput` binds its editing through actions in its own key
        // context and leaves these to bubble, which is the same arrangement
        // the command palette uses.
        .on_key_down(cx.listener(Pane::handle_launcher_key_down))
        // Clicking anywhere in the body puts the caret back in the filter, so
        // there is one focus target in here and no dead region.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                this.launcher_query
                    .read(cx)
                    .focus_handle
                    .clone()
                    .focus(window, cx);
            }),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::MD)
                .h(tok::row::HEADER)
                .px(tok::space::LG)
                .border_b_1()
                .border_color(ui.divider)
                .bg(ui.chrome)
                .child(
                    div()
                        .flex_none()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::ROW)
                        .text_color(ui.accent)
                        .child("\u{2315}"),
                )
                // **The wrapper states the size, because the field inherits
                // it.** `TextInput` shapes its content in
                // `window.text_style()`, so a mount that states nothing hands
                // it GPUI's default - which drew this query a third larger
                // than the rows it filters, and (until the widget took the ink
                // over) in a colour very nearly the pane's own. The list below
                // is `text::ROW`, and the query is the same words as the row
                // it will select.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(tok::text::ROW)
                        .child(query.clone()),
                ),
        )
        .child(list)
        .into_any_element()
}

impl gpui::Focusable for Pane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // The launcher is a pane's content like any other, and `FOCUS.md` is
        // explicit that it holds focus: "There is no 'no pane focused' state
        // and the launcher does not create one."
        if self.showing_launcher() {
            // The filter field, not a handle of the pane's own: it is the one
            // focus target in the launcher, so typing lands in it and the
            // three focus signals still name this pane.
            return self.launcher_query.read(cx).focus_handle.clone();
        }
        match self.tabs.get(self.selected_idx) {
            Some(TabContent::Terminal(t)) => t.read(cx).focus_handle(cx),
            Some(TabContent::Markdown(m)) => m.read(cx).focus_handle(cx),
            Some(TabContent::Diff(d)) => d.read(cx).focus_handle(cx),
            None => cx.focus_handle(),
        }
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_tab = self.tabs.get(self.selected_idx);
        let terminal_selected =
            matches!(selected_tab, Some(TabContent::Terminal(_))) && !self.showing_launcher();
        let launcher = self
            .showing_launcher()
            .then(|| render_pane_launcher(self, cx));
        let body = match (launcher, selected_tab) {
            (Some(launcher), _) => launcher,
            (None, Some(TabContent::Terminal(t))) => t.clone().into_any_element(),
            (None, Some(TabContent::Markdown(m))) => m.clone().into_any_element(),
            (None, Some(TabContent::Diff(d))) => d.clone().into_any_element(),
            (None, None) => div().size_full().into_any_element(),
        };
        let content_background = pane_content_background(
            &crate::theme::active_theme(),
            self.cached_config.windows_terminal_material_enabled(),
            terminal_selected,
        );

        // Drop-to-split: the content region hosts the drag-move
        // direction probe, the drop commit, and the blue preview overlay.
        // A unique group name (per pane entity) scopes `group_drag_over` so
        // only this pane's overlay reacts while a tab hovers its content.
        let group_name =
            SharedString::from(format!("pane-content-{}", cx.entity().entity_id().as_u64()));

        // Glide geometry: lerp the overlay from its previous region's rect to
        // the current one over a short ease, so the preview slides between
        // halves/center instead of hard-snapping (the cmux feel; Zed itself
        // snaps). The seq keys the animation ElementId, restarting the ease
        // each time the region changes; `split_rect` maps a `DropEdge` to an
        // absolute-pixel rect within the cached content size.
        let (cw, ch) = (
            self.overlay_pane_size.width.as_f32(),
            self.overlay_pane_size.height.as_f32(),
        );
        let from_rect = self.overlay_from;
        let to_rect = split_rect(self.drag_split_direction, cw, ch);
        let live_rect = self.overlay_current.clone();
        let overlay_anim_id = SharedString::from(format!(
            "pane-overlay-{}-{}",
            cx.entity().entity_id().as_u64(),
            self.overlay_seq
        ));

        // Translucent preview: full pane for center (move-into), or the half
        // the new split would occupy for an edge. `invisible()` by default and
        // only shown via `group_drag_over`, so it never paints - and so never
        // hit-tests / blocks terminal mouse input - unless a tab is being
        // dragged over this pane. Geometry is set per-frame by the
        // glide animator below (absolute px), not statically.
        // The overlay is also the drop target. Carrying `on_drop` here (rather
        // than on the parent `content`) is what gives the overlay its own
        // hitbox: GPUI's `should_insert_hitbox` keys off `drop_listeners`
        // (among others) but NOT off `group_drag_over`, so a handler-less div
        // never allocates a hitbox and its `group_drag_over` style is never
        // evaluated - i.e. the overlay would stay `invisible()` forever. This
        // mirrors Zed's `crates/workspace/src/pane.rs` drag-target div, which
        // is likewise `.invisible()` + `group_drag_over` + `on_drop`. The
        // hitbox is `HitboxBehavior::Normal`, so it never blocks the terminal's
        // mouse input behind it (Risk #3).
        let overlay_blue = Hsla::from(rgb(DROP_OVERLAY_BLUE));
        let overlay = div()
            .absolute()
            .bg(overlay_blue.opacity(DROP_OVERLAY_BACKGROUND_ALPHA))
            .rounded(OVERLAY_RADIUS)
            .border_2()
            .border_color(overlay_blue)
            .invisible()
            // A session dragged from the sidebar lights up the same overlay.
            .group_drag_over::<SessionDrag>(group_name.clone(), |s| s.visible())
            // A markdown file dragged from the Files sidebar - same overlay.
            .group_drag_over::<MarkdownFileDrag>(group_name.clone(), |s| s.visible())
            // A parked surface dragged out of the rail - same overlay, and the
            // same reason it must be here: the overlay carries the drop hitbox,
            // so a payload with no `group_drag_over` of its own would find
            // nothing to land on.
            .group_drag_over::<RailSurfaceDrag>(group_name.clone(), |s| s.visible())
            // Markdown drop: open the file via `FileView`, split toward the
            // previewed edge (or append as a tab for center). Tree mutation +
            // open live in `SplitlaneApp`, so emit + defer out of this callback
            // (entity re-entrancy, mirrors the session drop).
            .on_drop(
                cx.listener(move |this, drag: &MarkdownFileDrag, _window, cx| {
                    let edge = this.drag_split_direction.take();
                    cx.emit(PaneEvent::DropMarkdownSplit {
                        edge,
                        path: drag.path.clone(),
                    });
                    cx.notify();
                }),
            )
            // Session drop: spawn a fresh terminal running the resume command,
            // split toward the previewed edge (or append as a tab for center).
            // Tree mutation + spawn live in `SplitlaneApp`, so emit and defer out
            // of this callback (entity re-entrancy, Risk #1).
            .on_drop(cx.listener(move |this, drag: &SessionDrag, _window, cx| {
                let edge = this.drag_split_direction.take();
                cx.emit(PaneEvent::DropSessionSplit {
                    edge,
                    agent: drag.agent,
                    session_id: drag.session_id.clone(),
                    cwd: drag.cwd.clone(),
                });
                cx.notify();
            }))
            // A parked surface dropped out of the rail: it goes into THIS slot.
            // No edge is read - the design's drop onto a pane changes what
            // that pane shows, and a split is what the dashed strip is for.
            .on_drop(
                cx.listener(move |this, drag: &RailSurfaceDrag, _window, cx| {
                    this.drag_split_direction.take();
                    cx.emit(PaneEvent::DropRailSurface {
                        ws_idx: drag.ws_idx,
                        thread_id: drag.thread_id,
                    });
                    cx.notify();
                }),
            )
            // Glide between regions: lerp the absolute-px rect from the previous
            // region to the current one over a short ease-out. The animation
            // self-drives frames until it settles (no terminal-poll dependency),
            // and restarts whenever `overlay_anim_id` changes (region change).
            .with_animation(
                overlay_anim_id,
                Animation::new(Duration::from_millis(130)).with_easing(ease_out_quint()),
                move |overlay, delta| {
                    let lerp = |a: f32, b: f32| a + (b - a) * delta;
                    let raw = (
                        lerp(from_rect.0, to_rect.0),
                        lerp(from_rect.1, to_rect.1),
                        lerp(from_rect.2, to_rect.2),
                        lerp(from_rect.3, to_rect.3),
                    );
                    // Inset the visible box by a uniform margin so it floats
                    // inside the region (gap on every side, including the center
                    // line). The margin is applied *after* the lerp and is NOT
                    // stored in `live_rect` - seeding the next glide stays in the
                    // un-inset region space so `from`/`to` remain consistent.
                    let m = OVERLAY_MARGIN;
                    let cur = (
                        raw.0 + m,
                        raw.1 + m,
                        (raw.2 - 2.0 * m).max(0.0),
                        (raw.3 - 2.0 * m).max(0.0),
                    );
                    // Publish the *un-inset* live rect so the next region change
                    // can lerp from the box's actual mid-flight position, not
                    // the old target (kills the fast-crossing jump).
                    live_rect.set(raw);
                    overlay
                        .left(px(cur.0))
                        .top(px(cur.1))
                        .w(px(cur.2))
                        .h(px(cur.3))
                },
            );

        let content = div()
            .id("pane-content")
            .group(group_name)
            .relative()
            .flex_1()
            // `min_h_0` beside `flex_1`: a flex item's automatic minimum height
            // is its content's, and a surface that reports full content height -
            // the diff body does - would otherwise grow this slot past the pane
            // and push its own footer off the bottom edge instead of scrolling.
            .min_h_0()
            .size_full()
            .overflow_hidden()
            .bg(content_background)
            // Map the cursor within the content bounds to a split edge.
            // Stays on `content` (full pane) - the overlay shrinks to a half
            // when `dir = Some(edge)`, so probing there would miss the cursor
            // moving back toward the center band. `content` keeps its hitbox via
            // `.group(group_name)`.
            // Same edge-band probe for a session dragged out of the sidebar, so
            // it gets the identical blue preview (bridges the sessions sidebar).
            .on_drag_move::<SessionDrag>(cx.listener(
                |this, e: &DragMoveEvent<SessionDrag>, _window, cx| {
                    this.apply_drag_edge(e.bounds, e.event.position, cx);
                },
            ))
            // Identical edge-band probe for a markdown file dragged in.
            .on_drag_move::<MarkdownFileDrag>(cx.listener(
                |this, e: &DragMoveEvent<MarkdownFileDrag>, _window, cx| {
                    this.apply_drag_edge(e.bounds, e.event.position, cx);
                },
            ))
            // A rail surface reads NO edge band: it changes what this slot
            // shows rather than splitting it, and splitting is what the dashed
            // strip beside the panes is for. The probe still runs, because it
            // is what gives the drop overlay a size.
            .on_drag_move::<RailSurfaceDrag>(cx.listener(
                |this, e: &DragMoveEvent<RailSurfaceDrag>, _window, cx| {
                    this.apply_drag_whole_pane(e.bounds, cx);
                },
            ))
            .child(body)
            .child(overlay);

        // The border belongs to focus, which is what the design asks of it:
        // the focused slot is bordered, every other slot's border is reserved
        // but transparent, so taking focus never reflows the body.
        //
        // An earlier version painted this border with the attention
        // colour instead - a waiting agent glowed. That signal did not go away
        // with the border: it is the status dot in this slot's own header
        // (`render_single_surface_name`), the dot on the waiting tab's chip,
        // and the dot on its rail row. The border cannot carry both, and
        // between "an agent needs you" and "your keystrokes land here", only
        // the second has nowhere else to be said.
        let is_focused = self.focus_handle(cx).is_focused(window);
        let ui = tab_colors();
        let focus_border = ui.focus_border;
        let peek = self.render_peek_overlay(cx);
        let composer = self.render_composer_overlay(cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .relative()
            // The 1px band is reserved here and painted below: the corner
            // wedges have to sit between the surface and the border, or they
            // eat the very ring that says where focus is.
            .border_1()
            .border_color(gpui::transparent_black())
            .rounded(tok::radius::PANEL)
            .child(self.render_tab_bar(is_focused, window, cx))
            .child(content)
            .children(self.render_corner_masks())
            .child(
                div()
                    .absolute()
                    // Over the reserved band, not inside it: an absolute child
                    // is placed against the padding box, which starts where
                    // the border ends.
                    .top(px(-1.))
                    .left(px(-1.))
                    .right(px(-1.))
                    .bottom(px(-1.))
                    .border_1()
                    // `PANEL_RING`, not `PANEL`: this box is inset by -1px on
                    // every side, so an equal radius would put its arc inside
                    // the pane's own and open a sliver of the panes area
                    // between them at each corner.
                    .rounded(tok::radius::PANEL_RING)
                    // What an unfocused pane draws is `pane_border_idle`, and
                    // it is a role rather than a colour because the answer is
                    // not the same in both halves of the world. On Harbor
                    // Light it is a drawn edge and the **change** between the
                    // two borders is the signal, worth 3.08 : 1. On Harbor
                    // Dark it is nothing, and the signal is the outline
                    // appearing - because a drawn idle edge there measured
                    // 1.25 : 1 against the focused one, which is a pair the
                    // eye cannot tell apart at all. The band above is still
                    // reserved either way, so focus never reflows the body.
                    .border_color(if is_focused {
                        focus_border
                    } else {
                        ui.pane_border_idle
                    }),
            )
            .children(peek)
            // Broadcast-group stripe - a DISTINCT left-edge
            // element; the pane border slot above is focus's. Absolutely
            // positioned so it never perturbs the tab/content flex chain.
            //
            // It is **dashed**, 2px, and that is the whole point, as the
            // design has it: a SOLID accent bar on a pane edge is one of the three
            // focus signals, and this bar answers a different question. Same
            // colour, unmistakably not the same claim.
            //
            // The element is wider than the border it draws. GPUI paints a
            // quad's border on the half of the box nearest that edge, so a
            // 2px-wide box carrying a 2px left border paints about one pixel
            // and leaves the other transparent; at BROADCAST_STRIPE_BOX wide
            // the 2px border lands whole and the rest of the box stays empty.
            // Top and bottom inset by the panel radius so the square-cornered
            // stripe never pokes out of the pane's rounded corner.
            .when_some(self.broadcast_stripe, |d, idx| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(tok::radius::PANEL)
                        .bottom(tok::radius::PANEL)
                        .w(px(BROADCAST_STRIPE_BOX))
                        .border_l(px(BROADCAST_STRIPE_WIDTH))
                        .border_dashed()
                        .border_color(tab_colors().group_color(idx)),
                )
            })
            .children(composer)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContextFact, MAX_TAB_TITLE_LEN, pane_content_background, peek_badge_line,
        tab_bar_background, truncate_tab_title,
    };

    #[test]
    fn a_count_looks_like_a_count_at_every_size() {
        // No per cent sign and no track: there is nothing yet to be a share of,
        // and a number that looks like a share while being a count is the
        // failure this meter was withdrawn for once already. The `ctx` is
        // doing work - phase one has no arc beside the number to say what it
        // is about.
        let at = |tokens| ContextFact {
            tokens,
            ceiling: None,
            compaction: None,
            compactions_seen: 0,
        };
        assert_eq!(at(683_044).header_label(false), "683k ctx");
        assert_eq!(at(1_200_000).header_label(true), "1200k ctx");
        assert_eq!(at(940).header_label(false), "940 ctx");
    }

    #[test]
    fn the_full_form_states_both_numbers_in_one_unit() {
        // So a reader compares two figures instead of converting between them.
        let with = |tokens, ceiling| ContextFact {
            tokens,
            ceiling: Some(ceiling),
            compaction: None,
            compactions_seen: 0,
        };
        assert_eq!(
            with(683_044, 1_002_336).header_label(true),
            "0.68M / 1.00M ctx"
        );
        assert_eq!(with(683_044, 920_000).header_label(true), "683k / 920k ctx");
        // The short form drops the word: the arc is beside it saying what it is.
        assert_eq!(with(683_044, 1_002_336).header_label(false), "683k");
    }

    #[test]
    fn the_tooltip_says_what_the_header_cannot_and_not_what_it_already_does() {
        // No share: the arc draws it and the full form prints it. What is left
        // is the exact numbers, where the ceiling came from, and the count that
        // explains the sawtooth.
        let known = ContextFact {
            tokens: 683_044,
            ceiling: Some(1_002_336),
            compaction: None,
            compactions_seen: 3,
        };
        let lines = known.tooltip_lines();
        assert!(lines[0].starts_with("683"), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("ceiling")), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l == "where this session last compacted on its own"),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l == "compacted 3 times"), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains('%')),
            "the share is not repeated"
        );

        let unknown = ContextFact {
            ceiling: None,
            ..known
        };
        assert!(
            unknown
                .tooltip_lines()
                .iter()
                .any(|l| l == "no ceiling until this session compacts on its own")
        );
    }

    #[test]
    fn the_ring_is_on_until_a_turn_puts_something_back() {
        // It means "that drop was a compaction, not a fall in spending", so it
        // lives exactly as long as the two can be confused.
        let compaction = |turn_since| crate::claude_sessions::CompactionSeen {
            at: 1_000,
            pre_tokens: 1_000_132,
            post_tokens: 9_399,
            automatic: true,
            turn_since,
        };
        let fact = |turn_since| ContextFact {
            tokens: 9_399,
            ceiling: Some(1_000_132),
            compaction: Some(compaction(turn_since)),
            compactions_seen: 1,
        };
        assert!(fact(false).just_compacted());
        assert!(!fact(true).just_compacted());
        // And while it is on, the tooltip says how long ago and from what.
        assert!(
            fact(false)
                .tooltip_lines()
                .iter()
                .any(|l| l.starts_with("compacted ") && l.contains('→')),
            "{:?}",
            fact(false).tooltip_lines()
        );
    }

    #[test]
    fn a_context_past_its_measured_ceiling_fills_the_ring_rather_than_overflowing_it() {
        // The ceiling is where the *last* automatic compaction fired, so a
        // session can legitimately sit past it for a turn or two before the
        // next one. A ring drawn past full would be a rendering bug reporting
        // itself as a reading.
        let over = ContextFact {
            tokens: 1_100_000,
            ceiling: Some(1_000_000),
            compaction: None,
            compactions_seen: 0,
        };
        assert_eq!(over.fraction(), Some(1.0));
    }

    #[test]
    fn terminal_material_scopes_tab_bar_to_windows() {
        let ui = crate::theme::ui_colors_with(&crate::theme::one_dark());

        assert_eq!(tab_bar_background(&ui, false, false), ui.slot_header);
        if cfg!(target_os = "windows") {
            assert_eq!(tab_bar_background(&ui, true, false).a, 0.0);
            // Native material outranks focus: both slots share one backdrop,
            // and the border plus the rail's bar still say which is which.
            assert_eq!(tab_bar_background(&ui, true, true).a, 0.0);
        } else {
            assert_eq!(tab_bar_background(&ui, true, false), ui.slot_header);
        }
    }

    #[test]
    fn the_focused_slot_header_lifts() {
        let ui = crate::theme::ui_colors_with(&crate::theme::harbor_dark());

        assert_eq!(tab_bar_background(&ui, false, true), ui.slot_header_focus);
        assert_ne!(
            tab_bar_background(&ui, false, true),
            tab_bar_background(&ui, false, false)
        );
    }

    #[test]
    fn terminal_material_scopes_content_to_windows_terminal_tabs() {
        let theme = crate::theme::one_dark();

        assert_eq!(
            pane_content_background(&theme, true, false),
            theme.background
        );
        assert_eq!(
            pane_content_background(&theme, false, true),
            theme.background
        );

        let material = pane_content_background(&theme, true, true);
        #[cfg(target_os = "windows")]
        assert_eq!(material.a, 0.0);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(material, theme.background);
    }

    #[test]
    fn peek_badge_takes_first_line_bounded() {
        assert_eq!(
            peek_badge_line("Allow `cargo test`?\ndetails…"),
            "Allow `cargo test`?"
        );
        let long = "x".repeat(120);
        let badge = peek_badge_line(&long);
        assert_eq!(badge.chars().count(), 81, "80 chars + ellipsis");
        assert!(badge.ends_with('…'));
        assert_eq!(peek_badge_line(""), "");
        // Multibyte safety: counts chars, not bytes.
        let accents = "é".repeat(100);
        assert!(peek_badge_line(&accents).ends_with('…'));
    }

    #[test]
    fn short_titles_pass_through_unchanged() {
        assert_eq!(truncate_tab_title("README.md"), "README.md");
        assert_eq!(truncate_tab_title("Terminal"), "Terminal");
    }

    #[test]
    fn exactly_max_chars_is_not_truncated() {
        let s: String = "x".repeat(MAX_TAB_TITLE_LEN);
        assert_eq!(truncate_tab_title(&s), s);
    }

    #[test]
    fn over_max_gets_ellipsis() {
        // 25 chars in -> 24 chars out (23 head + ellipsis).
        let input = "doc-opencode-sessions.mdX";
        let out = truncate_tab_title(input);
        assert_eq!(out.chars().count(), MAX_TAB_TITLE_LEN);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn multibyte_utf8_does_not_panic() {
        // Earlier byte-slice path (`&raw[..23]`) panicked when index 23
        // landed in the middle of an accented or CJK char. The char-based
        // implementation must stay sound.
        let input = "événement-très-très-long-fichier.md"; // many multibyte chars
        let out = truncate_tab_title(input);
        assert_eq!(out.chars().count(), MAX_TAB_TITLE_LEN);
        assert!(out.ends_with('…'));
        let cjk = "プロジェクト・パネフロー・テスト・ドキュメント.md";
        let out = truncate_tab_title(cjk);
        assert_eq!(out.chars().count(), MAX_TAB_TITLE_LEN);
    }

    #[test]
    fn cwd_label_uses_last_path_component() {
        let cwd = std::env::temp_dir().join("splitlane-tab-title");

        assert_eq!(
            super::Pane::cwd_label(&cwd.to_string_lossy()),
            Some("splitlane-tab-title".into())
        );
    }

    #[test]
    fn agent_title_detection_uses_exact_command_token() {
        assert_eq!(
            super::Pane::agent_title_from_terminal_title("codex"),
            Some("Codex")
        );
        assert_eq!(
            super::Pane::agent_title_from_terminal_title("codex.exe"),
            Some("Codex")
        );
        assert_eq!(
            super::Pane::agent_title_from_terminal_title("user@host: /repo/codex-adapter"),
            None,
            "repo names must not be mistaken for agent processes"
        );
    }
}
