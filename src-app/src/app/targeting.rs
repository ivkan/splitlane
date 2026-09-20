//! The targeting ladder: which pane a surface opens into.
//!
//! `FOCUS.md` answers two questions that are constantly confused with each
//! other - *which pane does a session open into*, and *which pane ends up
//! focused*. This module owns the first one, and it owns it alone: every
//! entry that opens a surface (a rail row, `+ agent` / `+ shell`, a palette
//! row, `⌘D`, a `.md` in the tree, the waiting chip) asks
//! [`SplitlaneApp::place_surface`] rather than reaching for a pane itself.
//!
//! The ladder has four rungs and stops at the first one that exists:
//!
//! 1. **A pane of the same kind.** An agent goes to the agent pane, a shell to
//!    the shell pane, a `.md` to the markdown pane. This is what keeps the
//!    window's shape stable across dozens of switches.
//! 2. **An empty pane** - one with no surface in it, which in practice is a
//!    pane showing the launcher. Its whole purpose is to be filled.
//! 3. **A new pane**, if there are fewer than [`crate::layout::MAX_PANES`] and
//!    the split still leaves every pane at least [`MIN_PANE_FOR_SPLIT`] on the
//!    long axis.
//! 4. **The focused pane**, replaced.
//!
//! The return type is an enum rather than an `Entity<Pane>` because rung 3
//! does not *pick* a pane, it *creates* one - and a caller that flattened the
//! two would have to re-derive "was there room?" to know what it just did.
//!
//! **Replacement is the ordinary case, not a fallback.** With the limit being a
//! width rather than a count, a laptop reaches two panes and stays there, so
//! rung 1 reads as a preference and rung 4 is where most opens land.
//!
//! It never loses a session, and the reason has moved. It used to be
//! "a pane here is a tab strip, so the surface is one click away in the same
//! header". Panes hold one surface now, and the surface is one click away **in
//! the rail**: `agents_terminal_view_cache` owns these views and a pane only
//! points at them, and that cache refuses to evict a terminal which has not
//! exited - it goes over budget instead. So a replaced agent keeps running and
//! keeps its scrollback, and its rail row goes from filled to dimmed while
//! keeping its status dot.

use gpui::{App, Context, Entity};

use crate::SplitlaneApp;
use crate::layout::SplitDirection;
use crate::pane::{Pane, TabContent};

/// Rung 3's threshold: the narrowest a pane may end up as a *result* of
/// opening something.
///
/// The design's number - "the split still leaves every pane at least 320px on
/// its long axis. Below that width panes start dropping their header labels,
/// so a click meant to open one session would degrade two others."
///
/// This is deliberately NOT [`crate::layout::MIN_PANE_SIZE`] (300), which
/// clamps a divider *drag*: that one is a floor on what the user may do on
/// purpose, this one is a floor on what a click may do to them. Two numbers,
/// two roles - merging them would make one of the two rules a lie.
pub(crate) const MIN_PANE_FOR_SPLIT: f32 = 320.0;

/// The four kinds a pane can be of. Four kinds, three panes - which is why
/// rung 4 exists and needs no special case.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SurfaceKind {
    /// An agent session: a PTY running a CLI agent, or its rendered
    /// transcript. Both faces are one surface.
    Agent,
    /// A shell - the container's own terminal, agent-less.
    Shell,
    /// The container's git diff.
    Diff,
    /// A rendered markdown file.
    Markdown,
}

/// Where the ladder landed.
pub(crate) enum PaneTarget {
    /// Rungs 1, 2 and 4: a pane that is already on screen.
    Existing(Entity<Pane>),
    /// Rung 3: append a pane at the end of the tree and re-split evenly.
    NewPane,
}

/// Why another pane is refused.
///
/// The refusal has a **reason**, and the reason has to reach a person: it is
/// settled that a control gated by a continuous quantity stays in place, dimmed,
/// and says why - so "no" is not enough, the sentence is part of the answer.
/// Two refusals, two sentences, and they are written here rather than at the
/// two call sites (the toolbar's tooltip and the chord's toast) so the button
/// and the keystroke cannot come to disagree about what the limit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneRefusal {
    /// Four is as many panes as there are kinds for one to be.
    Ceiling,
    /// Every pane would come out under [`MIN_PANE_FOR_SPLIT`]. Carries the
    /// count there would have been, because that is the number the sentence
    /// names - a person reading "3 panes would each be under 320px" can check
    /// it against what is on screen.
    TooNarrow { would_be: usize },
    /// The same width refusal, in the case where a **2x2 would fit**.
    ///
    /// A refusal may name the form that would work, and that is the
    /// most useful thing it can say. It is still a refusal - it does not change
    /// the arrangement, it says which control does. Where the grid does not fit
    /// either, the plain [`PaneRefusal::TooNarrow`] wording is used instead,
    /// because a suggestion the reader cannot act on is worse than silence.
    ///
    /// `stacked` carries the word for the arrangement the panes are in now, so
    /// the sentence describes what was refused rather than assuming a row.
    TooNarrowGridFits { would_be: usize, stacked: bool },
    /// There is no pane to add one beside. Closing the last pane is a state
    /// this app has - the empty area names the doors out of it - and this
    /// control is not one of them: a split needs something to split.
    ///
    /// It exists because the other two sentences are **false** here, and a
    /// cross-vendor pass caught both. A container with no tree answered
    /// `Ceiling`, so a project showing zero panes was told "4 panes is the
    /// limit"; and `panes == 0` answered `TooNarrow { would_be: 1 }`, a claim
    /// that one pane would be under 320px on a window of any width at all.
    /// Both are exactly the thing a stated limit must never be - a number the
    /// reader cannot check against the screen.
    NoPanes,
    /// A pane holding nothing is already on screen.
    ///
    /// **A refusal names what is already on screen before it names the
    /// ceiling**, which is the grid-refusal rule turned one step outward: that one
    /// let a refusal name the *form* that would fit, and this one lets it name
    /// the *pane* that is already there. `4 panes is the limit` is true and
    /// reads as false to somebody looking at two empty cells, which is the
    /// worst kind of true - and telling them a number when the answer is
    /// "it is right there" spends their attention on arithmetic.
    ///
    /// The condition is the empty pane and **not the grid**: a row of three
    /// with one launcher gets the same sentence for the same reason. So it is
    /// asked first, ahead of every other refusal, and it is the only one of the
    /// five that needs to look inside a pane.
    EmptyPaneAlready,
}

impl PaneRefusal {
    /// What to tell a person, in the one form both the tooltip and the toast
    /// use.
    pub(crate) fn sentence(self) -> String {
        match self {
            Self::Ceiling => format!("{} panes is the limit", crate::layout::MAX_PANES),
            Self::TooNarrow { would_be } => format!(
                "No room - {would_be} panes would each be under {}px",
                MIN_PANE_FOR_SPLIT as u32
            ),
            // "four" as a word, which is the design's own sentence, and it can
            // stay a word: a 2x2 has four cells by construction, so this is not
            // a number that could drift out of step with a constant.
            Self::TooNarrowGridFits { would_be, stacked } => {
                let arrangement = if stacked { "stacked" } else { "side by side" };
                format!("No room for {would_be} {arrangement} - Grid fits four")
            }
            Self::NoPanes => "No panes here yet - open one from the area below".to_string(),
            Self::EmptyPaneAlready => "An empty pane is already open".to_string(),
        }
    }
}

/// Rung 3's condition, with no tree and no window to read - so the arithmetic
/// can be checked without an app.
///
/// `long_axis_px` is the panes area along the axis the split runs, `panes` how
/// many are on screen now. Refuses a degenerate measurement rather than
/// guessing: a frame that has not laid out yet must not be read as "there is
/// room". It reads as the width refusal, which is what it would become as soon
/// as the frame is real - and it lasts one frame, which is less than a tooltip
/// takes to appear. That is the one sentence here whose number was not
/// computed; it is named rather than hidden, and it is the reason `panes == 0`
/// is answered above it rather than folded into it.
pub(crate) fn refuse_another_pane(long_axis_px: f32, panes: usize) -> Option<PaneRefusal> {
    if panes >= crate::layout::MAX_PANES {
        return Some(PaneRefusal::Ceiling);
    }
    if panes == 0 {
        return Some(PaneRefusal::NoPanes);
    }
    let after = panes + 1;
    if !long_axis_px.is_finite() || long_axis_px <= 0.0 {
        return Some(PaneRefusal::TooNarrow { would_be: after });
    }
    let gaps = crate::layout::DIVIDER_PX * (after - 1) as f32;
    if (long_axis_px - gaps) / after as f32 >= MIN_PANE_FOR_SPLIT {
        None
    } else {
        Some(PaneRefusal::TooNarrow { would_be: after })
    }
}

/// Whether a 2x2 would leave every cell readable, from the panes area alone -
/// so, like [`refuse_another_pane`], the arithmetic can be checked without an
/// app.
///
/// **Both axes**, and that is the difference from the flat forms: a row is
/// refused on its long axis because the short one is not divided, and a grid
/// divides both. The same 320px on each, because the number is "narrower than a
/// pane can state what it is showing" and a header cut off at the bottom says
/// no more than one cut off at the right.
///
/// It is worth seeing that this is self-consistent with the row rule at the
/// bottom end: a grid cell and one of two side-by-side panes are the **same
/// width**, `(w - DIVIDER_PX) / 2`. So where a second pane does not fit, this
/// answers no as well, and the suggestion can never be offered in place of a
/// refusal it would not solve.
/// What to say when a 2x2 will not fit.
///
/// One home, for the same reason `PaneRefusal::sentence` is one: the dimmed
/// `Grid` segment's tooltip and the action's toast are two ways of meeting the
/// same wall, and a person who met one after the other must not be told two
/// different things about it. It is not a `PaneRefusal` variant because those
/// answer "can another pane be added"; this answers "can this form be chosen",
/// which is a different question with a different control.
pub(crate) const GRID_REFUSAL: &str = "No room for a grid - four cells would each be under 320px";

pub(crate) fn grid_fits(area: (f32, f32)) -> bool {
    let (w, h) = area;
    if !w.is_finite() || !h.is_finite() {
        return false;
    }
    let half = |axis: f32| (axis - crate::layout::DIVIDER_PX) / 2.0;
    half(w) >= MIN_PANE_FOR_SPLIT && half(h) >= MIN_PANE_FOR_SPLIT
}

impl SplitlaneApp {
    /// Whether the person can currently **see** this agent surface.
    ///
    /// The question a notification has to ask, and it is not "is the Splitlane
    /// window focused". That gate assumed "if you are in the app you will see
    /// it", which is true of one pane and false of exactly the thing this app
    /// is for: four sessions across three projects, of which the panes show at
    /// most a few and the rail rows of a folded project show none. A completion
    /// in a project you are not looking at went by in silence while the app sat
    /// in front of you, because the app was in front of you.
    ///
    /// Three conditions, all necessary. The window is the OS's answer and
    /// covers "you are in a browser". The **active** container is ours: a
    /// surface in another project is not on screen however many panes it has.
    /// And the pane's **active tab** rather than any tab, which is the same
    /// rule `pane_kind` and `FocusedSurface` already use - a pane shows one
    /// surface at a time.
    ///
    /// An **unresolved** surface (`None`) counts as unseen. That is the safe
    /// direction and it is a choice: the case is a hook frame whose pid never
    /// resolved to a pane, so the app knows least about it, and a completion
    /// nobody hears about is a worse failure than a notification for something
    /// that happened to be visible.
    pub(crate) fn surface_is_seen(&self, surface_id: Option<u64>, cx: &App) -> bool {
        let Some(surface_id) = surface_id else {
            return false;
        };
        if !crate::agents::notifications::window_active() {
            return false;
        }
        self.active_workspace()
            .and_then(|container| container.root.as_ref())
            .is_some_and(|root| {
                root.collect_leaves().iter().any(|pane| {
                    let pane = pane.read(cx);
                    pane.tabs
                        .get(pane.selected_idx)
                        .and_then(TabContent::as_terminal)
                        .is_some_and(|view| view.entity_id().as_u64() == surface_id)
                })
            })
    }

    /// What to **call** the surface a notification is about - the same string
    /// its rail row carries.
    ///
    /// The sibling of [`Self::surface_is_seen`], and it exists because the two
    /// notification arms knew different amounts about the same thing. A frame
    /// whose PTY is owned by a `Thread` arrives with the surface's own title;
    /// one owned by a `Workspace` - an agent somebody started by hand in a
    /// shell pane, which no launcher created a record for - arrived with the
    /// **project's** title, so two such agents in one project sent two pings
    /// that named neither. The surface id was already in hand at those sites,
    /// resolved for the seen-check and then not used for the name.
    ///
    /// **Every container, not just the active one.** `surface_is_seen` asks
    /// only the active container because being outside it is what makes a
    /// surface unseen; naming is the opposite question, and a notification
    /// fires precisely for a surface the person is not looking at.
    ///
    /// `None` when the id resolves to no pane on screen. That is a real state
    /// (a hook frame whose pid never bound to a pane), and the caller falls back
    /// to the project: a name it cannot verify is worse than the wider one it
    /// can.
    pub(crate) fn surface_name(&self, surface_id: Option<u64>, cx: &App) -> Option<String> {
        let surface_id = surface_id?;
        self.workspaces.iter().find_map(|container| {
            let root = container.root.as_ref()?;
            root.collect_leaves().iter().find_map(|pane| {
                let pane_ref = pane.read(cx);
                let tab = pane_ref
                    .tabs
                    .iter()
                    .find(|tab| {
                        TabContent::as_terminal(tab)
                            .is_some_and(|view| view.entity_id().as_u64() == surface_id)
                    })?
                    .clone();
                // The rail's own name for this session when there is a record,
                // and the tab's label when there is not - the order
                // `attention_queue` already uses, so the popover and the
                // notification cannot call one surface two things.
                let name = TabContent::as_terminal(&tab)
                    .and_then(|view| view.read(cx).agent_thread_id)
                    .and_then(|tid| container.threads.iter().find(|t| t.id == tid))
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| crate::pane::Pane::tab_title(&tab, cx));
                crate::project::clean_sidebar_title(&name).or(Some(name))
            })
        })
    }

    /// The same question asked of an agent surface by its **record** id.
    ///
    /// Two identifiers reach the notification sites - a session carries the
    /// terminal's entity id, a `Thread` carries its own - so the question has
    /// two doors and one answer.
    pub(crate) fn thread_is_seen(&self, thread_id: u64, cx: &App) -> bool {
        crate::agents::notifications::window_active()
            && self.agent_surface_in_slot(self.active_idx, thread_id, cx)
    }

    /// Whether the toolbar's `Grid` segment can be pressed, against the window
    /// as it is right now. Dimmed in place when not: a control gated
    /// by a continuous quantity does not appear and disappear as it changes.
    pub(crate) fn grid_fits_now(&self) -> bool {
        grid_fits(self.panes_area.get())
    }

    /// Rung 3's question, asked against the window as it is right now.
    ///
    /// **One asker used to be three.** The ladder measured width here,
    /// the Split button compared a count, and the drop strip had its own idea -
    /// so "is there room?" got different answers depending on how it was asked,
    /// and Split created panes the ladder refused. They all call this.
    pub(crate) fn room_for_another_pane_now(&self, cx: &App) -> bool {
        self.refuse_another_pane_now(cx).is_none()
    }

    /// The same question with its reason, for the two places that have to say
    /// one: the toolbar's dimmed control and the chord's toast.
    ///
    /// A container with no tree at all is [`PaneRefusal::NoPanes`], and that
    /// **is** a state a person can be looking at: closing the last pane leaves
    /// a container with none, and the empty area names the doors out of it. It
    /// used to answer with the ceiling's sentence, so a project showing zero
    /// panes was told that four is the limit.
    pub(crate) fn refuse_another_pane_now(&self, cx: &App) -> Option<PaneRefusal> {
        let Some(root) = self.active_workspace().and_then(|ws| ws.root.as_ref()) else {
            return Some(PaneRefusal::NoPanes);
        };
        // First, ahead of every limit: the rule is that a refusal names
        // what is already on screen before it names a ceiling. A pane showing
        // the launcher **over** a live surface is not one of these - it has a
        // surface in it, and the question here is whether an empty pane exists,
        // not whether a launcher is drawn.
        if root
            .collect_leaves()
            .iter()
            .any(|pane| pane.read(cx).tabs.is_empty())
        {
            return Some(PaneRefusal::EmptyPaneAlready);
        }
        let area = self.panes_area.get();
        let (w, h) = area;
        let stacked = matches!(root.root_direction(), Some(SplitDirection::Horizontal));
        let long_axis = if stacked {
            h
        } else {
            // Side by side is the direction a first split takes, so a single
            // pane answers with the width too.
            w
        };
        let refusal = refuse_another_pane(long_axis, root.leaf_count())?;
        // The width refusal is allowed to name the form that would
        // fit. Only that one - the ceiling is not a thing another arrangement
        // solves, and "no panes here yet" is not a limit at all. A grid can
        // never be suggested from inside one, because a grid holds four leaves
        // and the ceiling answers first.
        Some(match refusal {
            PaneRefusal::TooNarrow { would_be } if grid_fits(area) => {
                PaneRefusal::TooNarrowGridFits { would_be, stacked }
            }
            other => other,
        })
    }

    /// What kind of surface `pane` is showing, read from its **active** tab.
    ///
    /// The active tab and not any tab: a pane shows one surface at a time, and
    /// a background tab is no more "what this pane is" than a background
    /// window is what the screen shows.
    pub(crate) fn pane_kind(&self, pane: &Entity<Pane>, cx: &App) -> Option<SurfaceKind> {
        let pane = pane.read(cx);
        Some(match pane.tabs.get(pane.selected_idx)? {
            TabContent::Markdown(_) => SurfaceKind::Markdown,
            TabContent::Diff(_) => SurfaceKind::Diff,
            TabContent::Terminal(view) => match view.read(cx).agent_thread_id {
                // A surface with a rail row: an agent session, unless the row
                // says it is a plain shell of the container.
                Some(thread_id) => {
                    if self
                        .thread_by_id(thread_id)
                        .is_some_and(crate::app::agents_sidebar::is_shell_surface)
                    {
                        SurfaceKind::Shell
                    } else {
                        SurfaceKind::Agent
                    }
                }
                // A pane a split created: a shell and nothing else.
                None => SurfaceKind::Shell,
            },
        })
    }

    /// The agent surface record `thread_id` names, in whatever container holds
    /// it.
    pub(crate) fn thread_by_id(&self, thread_id: u64) -> Option<&crate::project::Thread> {
        self.workspaces
            .iter()
            .find_map(|container| container.threads.iter().find(|t| t.id == thread_id))
    }

    /// The same record, to write to. Ids come from one process-wide counter,
    /// so the first match is the only match.
    pub(crate) fn thread_by_id_mut(
        &mut self,
        thread_id: u64,
    ) -> Option<&mut crate::project::Thread> {
        self.workspaces
            .iter_mut()
            .find_map(|container| container.threads.iter_mut().find(|t| t.id == thread_id))
    }

    /// The ladder itself. `None` only when the container has no tree at all,
    /// which is the one state with no pane to answer with.
    pub(crate) fn target_pane_for(
        &self,
        ws_idx: usize,
        kind: SurfaceKind,
        cx: &App,
    ) -> Option<PaneTarget> {
        let container = self.workspaces.get(ws_idx)?;
        // A container with no tree at all - restored from a file that had no
        // layout, or emptied by closing its last pane. The surface becomes its
        // first pane rather than being refused: "every surface is a pane's
        // content" has no arm for a surface with nowhere to be.
        let Some(root) = container.root.as_ref() else {
            return Some(PaneTarget::NewPane);
        };
        let leaves = root.collect_leaves();
        if leaves.is_empty() {
            return Some(PaneTarget::NewPane);
        }
        // 1. A pane of the same kind.
        if let Some(pane) = leaves
            .iter()
            .find(|pane| self.pane_kind(pane, cx) == Some(kind))
        {
            return Some(PaneTarget::Existing(pane.clone()));
        }
        // 2. An empty pane - the launcher's.
        if let Some(pane) = leaves
            .iter()
            .find(|pane| self.pane_kind(pane, cx).is_none())
        {
            return Some(PaneTarget::Existing(pane.clone()));
        }
        // 3. A new pane, if the split leaves every pane wide enough to still
        //    read its own header.
        if self.room_for_another_pane_now(cx) {
            return Some(PaneTarget::NewPane);
        }
        // 4. The focused pane. `focused_pane_now` is the pair `⌃⇥` is answered
        //    from, written every frame - so the ladder needs no `Window` and
        //    gives the same answer a keystroke would.
        let focused = self
            .focused_pane_now
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .filter(|pane| root.contains_leaf(pane))
            .or_else(|| leaves.first().cloned())?;
        Some(PaneTarget::Existing(focused))
    }

    /// Open `tab` in the pane the ladder names, and leave it focused.
    ///
    /// This is the one placement path. Focus goes through `pending_pane_focus`
    /// rather than a `Window`, because most of the entries that open a surface
    /// (a rail click, a menu item, a palette row) have no window to hand over;
    /// `render` drains the channel on the next frame, which is the same
    /// mechanism the drop-to-split has always used.
    pub(crate) fn place_surface(
        &mut self,
        ws_idx: usize,
        kind: SurfaceKind,
        tab: TabContent,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Pane>> {
        let target = self.target_pane_for(ws_idx, kind, cx)?;
        let pane = match target {
            PaneTarget::Existing(pane) => {
                // Shows, which replaces. Rungs 1, 2 and 4 all land here, and on
                // a laptop rung 4 is most of them - see the note above on why
                // that costs the user their place on screen and not their work.
                pane.update(cx, |pane, cx| pane.show_surface(tab, cx));
                pane
            }
            PaneTarget::NewPane => self.append_pane_with(ws_idx, tab, cx)?,
        };
        self.active_idx = ws_idx;
        self.pending_pane_focus = Some(pane.clone());
        self.reroot_files_tree(cx);
        self.save_session(cx);
        cx.notify();
        Some(pane)
    }

    /// Raise a surface that is already open in one of `ws_idx`'s panes and
    /// give that pane focus.
    ///
    /// `FOCUS.md`: "If the session is already open, nothing is replaced - its
    /// pane simply takes focus." Every entry asks this before the ladder,
    /// because the ladder has no rung for "it is already here" and would
    /// happily open a second copy on top of the first.
    pub(crate) fn reveal_tab_in_panes(
        &mut self,
        ws_idx: usize,
        matches: impl Fn(&TabContent, &App) -> bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let found = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
            .and_then(|root| {
                root.collect_leaves().into_iter().find_map(|pane| {
                    let idx = pane.read(cx).tabs.iter().position(|tab| matches(tab, cx))?;
                    Some((pane, idx))
                })
            });
        let Some((pane, tab_idx)) = found else {
            return false;
        };
        pane.update(cx, |pane, cx| {
            pane.selected_idx = tab_idx;
            cx.notify();
        });
        self.active_idx = ws_idx;
        self.pending_pane_focus = Some(pane);
        self.reroot_files_tree(cx);
        cx.notify();
        true
    }

    /// An empty pane appended at the end of the tree - a pane whose content is
    /// the launcher, because that is what "no session in it" draws.
    pub(crate) fn append_empty_pane(
        &mut self,
        ws_idx: usize,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Pane>> {
        let container = self.workspaces.get(ws_idx)?;
        let anchor = container
            .root
            .as_ref()
            .and_then(|root| root.collect_leaves().into_iter().next_back());
        let direction = container
            .root
            .as_ref()
            .and_then(|root| root.root_direction())
            .unwrap_or(SplitDirection::Vertical);
        let pane = self.create_pane_with_existing_tabs(Vec::new(), 0, cx);
        let Some(anchor) = anchor else {
            self.workspaces.get_mut(ws_idx)?.root =
                Some(crate::layout::LayoutTree::Leaf(pane.clone()));
            return Some(pane);
        };
        let inserted = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|container| container.root.as_mut())
            .is_some_and(|root| root.split_at_pane(&anchor, direction, pane.clone()));
        if !inserted {
            return None;
        }
        if let Some(root) = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
        {
            root.equalize_ratios();
        }
        Some(pane)
    }

    /// Rung 3's half: a pane appended at the end of the tree, re-split evenly.
    ///
    /// Appended and not split off the focused pane, because rung 3 is about
    /// *how many panes are on screen*, not about a neighbour - the same
    /// position the third-pane drop strip aims at.
    pub(crate) fn append_pane_with(
        &mut self,
        ws_idx: usize,
        tab: TabContent,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Pane>> {
        let container = self.workspaces.get(ws_idx)?;
        let anchor = container
            .root
            .as_ref()
            .and_then(|root| root.collect_leaves().into_iter().next_back());
        let direction = container
            .root
            .as_ref()
            .and_then(|root| root.root_direction())
            .unwrap_or(SplitDirection::Vertical);
        let pane = self.create_pane_with_existing_tab(tab, cx);
        let Some(anchor) = anchor else {
            // The container's first pane.
            self.workspaces.get_mut(ws_idx)?.root =
                Some(crate::layout::LayoutTree::Leaf(pane.clone()));
            return Some(pane);
        };
        let inserted = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|container| container.root.as_mut())
            .is_some_and(|root| root.split_at_pane(&anchor, direction, pane.clone()));
        if !inserted {
            return None;
        }
        if let Some(root) = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
        {
            root.equalize_ratios();
        }
        Some(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_PANE_FOR_SPLIT, PaneRefusal, refuse_another_pane};

    /// The arithmetic as a bare yes/no. The refusal carries a reason because
    /// the interface has to state one; these tests are about the boundary, so
    /// they ask the shorter question.
    fn room_for_another_pane(long_axis_px: f32, panes: usize) -> bool {
        refuse_another_pane(long_axis_px, panes).is_none()
    }

    /// The ceiling stops counting even on a wall-sized display. Written
    /// against `MAX_PANES` and not against a number: it was `3` until the
    /// ceiling became four, and a literal here would have failed rather than moved.
    #[test]
    fn a_full_tree_never_grows() {
        assert!(!room_for_another_pane(4000.0, crate::layout::MAX_PANES));
        assert!(!room_for_another_pane(4000.0, crate::layout::MAX_PANES + 6));
        // And one below the ceiling, a wide enough window still has room -
        // otherwise the assertion above would pass for the wrong reason.
        assert!(room_for_another_pane(4000.0, crate::layout::MAX_PANES - 1));
    }

    #[test]
    fn a_degenerate_measurement_is_not_room() {
        assert!(!room_for_another_pane(0.0, 1));
        assert!(!room_for_another_pane(-10.0, 1));
        assert!(!room_for_another_pane(f32::NAN, 1));
        // A container with no panes has no pane to split off, either.
        assert!(!room_for_another_pane(4000.0, 0));
    }

    #[test]
    fn the_threshold_is_measured_after_the_split() {
        // Two panes of exactly 320 plus the divider between them.
        let exact = MIN_PANE_FOR_SPLIT * 2.0 + crate::layout::DIVIDER_PX;
        assert!(room_for_another_pane(exact, 1));
        assert!(!room_for_another_pane(exact - 1.0, 1));
    }

    #[test]
    fn a_third_pane_asks_for_three_shares_not_two() {
        // Wide enough for two panes at 320, nowhere near enough for three.
        let two_up = MIN_PANE_FOR_SPLIT * 2.0 + crate::layout::DIVIDER_PX;
        assert!(room_for_another_pane(two_up, 1));
        assert!(!room_for_another_pane(two_up, 2));
        let three_up = MIN_PANE_FOR_SPLIT * 3.0 + crate::layout::DIVIDER_PX * 2.0;
        assert!(room_for_another_pane(three_up, 2));
    }

    /// A grid cell and one of two side-by-side panes are the same width, so
    /// the two rules meet exactly at the boundary. That is what lets the
    /// refusal offer the grid without ever offering it where it would not help.
    #[test]
    fn a_grid_cell_is_exactly_one_of_two_side_by_side() {
        let two_up = MIN_PANE_FOR_SPLIT * 2.0 + crate::layout::DIVIDER_PX;
        assert!(super::grid_fits((two_up, two_up)));
        assert!(!super::grid_fits((two_up - 1.0, two_up)));
        // Both axes, and that is the whole difference from a row: a window wide
        // enough for four cells and too short for two rows of them is not a
        // grid, however wide it gets.
        assert!(!super::grid_fits((4000.0, two_up - 1.0)));
        assert!(!super::grid_fits((f32::NAN, two_up)));
        assert!(!super::grid_fits((two_up, f32::INFINITY)));
    }

    /// The three sentences are three different claims, and the middle one is
    /// the only one that names a way out.
    #[test]
    fn the_refusals_read_as_three_different_things() {
        assert_eq!(
            PaneRefusal::Ceiling.sentence(),
            format!("{} panes is the limit", crate::layout::MAX_PANES)
        );
        assert_eq!(
            PaneRefusal::TooNarrow { would_be: 3 }.sentence(),
            "No room - 3 panes would each be under 320px"
        );
        assert_eq!(
            PaneRefusal::TooNarrowGridFits {
                would_be: 3,
                stacked: false
            }
            .sentence(),
            "No room for 3 side by side - Grid fits four"
        );
        // The arrangement word follows the form the panes are actually in, so
        // the sentence describes what was refused rather than assuming a row.
        assert_eq!(
            PaneRefusal::TooNarrowGridFits {
                would_be: 3,
                stacked: true
            }
            .sentence(),
            "No room for 3 stacked - Grid fits four"
        );
        assert_eq!(
            PaneRefusal::EmptyPaneAlready.sentence(),
            "An empty pane is already open"
        );
    }

    /// The empty-pane rule, as a property of the sentences rather than of one of
    /// them: **a refusal names what is already on screen before it names a
    /// ceiling**, so the one refusal about a pane that exists may not read like
    /// the four that are about a limit. None of the others mentions a pane the
    /// reader can point at, and this one names nothing else.
    #[test]
    fn the_refusal_about_a_pane_that_exists_states_no_limit() {
        let sentence = PaneRefusal::EmptyPaneAlready.sentence();
        assert!(
            !sentence.contains("limit") && !sentence.chars().any(|c| c.is_ascii_digit()),
            "it points at the screen, it does not do arithmetic: {sentence}"
        );
    }

    /// The sentence names a number, and a number a person cannot check against
    /// the rule is worse than no number - so the rule's own value has to be in
    /// it. Written as a literal because it is a sentence, checked here because
    /// a literal is exactly what drifts.
    #[test]
    fn the_grid_refusal_names_the_number_the_rule_uses() {
        assert!(
            super::GRID_REFUSAL.contains(&format!("{}px", MIN_PANE_FOR_SPLIT as u32)),
            "the grid refusal must name {MIN_PANE_FOR_SPLIT}px: {}",
            super::GRID_REFUSAL
        );
    }

    #[test]
    fn the_drag_floor_and_the_split_floor_are_different_numbers() {
        // 300 clamps a divider drag, 320 decides whether to add a pane. A
        // change that merged them would make one of the two rules a lie.
        assert_ne!(MIN_PANE_FOR_SPLIT, crate::layout::MIN_PANE_SIZE);
    }

    /// The refusals are told apart, because they are different sentences to a
    /// person: one is about this window and can be fixed by dragging it, one is
    /// about the app and cannot, and one is not about a limit at all.
    #[test]
    fn a_refusal_says_which_limit_it_is() {
        assert_eq!(
            refuse_another_pane(4000.0, crate::layout::MAX_PANES),
            Some(PaneRefusal::Ceiling)
        );
        assert_eq!(
            refuse_another_pane(200.0, 1),
            Some(PaneRefusal::TooNarrow { would_be: 2 })
        );
        assert_eq!(refuse_another_pane(4000.0, 1), None);
    }

    /// No panes is not a limit, and saying either limit's sentence there is a
    /// number the reader cannot check against the screen. On a 4000px window
    /// "1 panes would each be under 320px" was false twice over.
    #[test]
    fn no_panes_is_its_own_answer() {
        for width in [0.0, 200.0, 4000.0] {
            assert_eq!(
                refuse_another_pane(width, 0),
                Some(PaneRefusal::NoPanes),
                "at {width}px"
            );
        }
        let sentence = PaneRefusal::NoPanes.sentence();
        assert!(!sentence.contains("320"), "{sentence}");
        assert!(
            !sentence.contains(&crate::layout::MAX_PANES.to_string()),
            "{sentence}"
        );
    }

    /// Every width sentence names at least two panes, so none of them can come
    /// out as "1 panes".
    #[test]
    fn a_width_sentence_never_names_one_pane() {
        for panes in 1..crate::layout::MAX_PANES {
            for width in [0.0, f32::NAN, 100.0] {
                if let Some(PaneRefusal::TooNarrow { would_be }) = refuse_another_pane(width, panes)
                {
                    assert!(would_be >= 2, "{panes} panes at {width}px");
                }
            }
        }
    }

    /// The number in the sentence is the count there **would have been**, so a
    /// person can check it against the panes on screen.
    #[test]
    fn the_width_sentence_names_the_count_it_refused() {
        let sentence = PaneRefusal::TooNarrow { would_be: 3 }.sentence();
        assert!(sentence.contains('3'), "{sentence}");
        assert!(
            sentence.contains(&format!("{}px", MIN_PANE_FOR_SPLIT as u32)),
            "{sentence}"
        );
        assert!(
            PaneRefusal::Ceiling
                .sentence()
                .contains(&crate::layout::MAX_PANES.to_string())
        );
    }

    /// An unlaid-out frame refuses rather than guessing, and it refuses on
    /// width - the answer it will have as soon as the frame is real.
    #[test]
    fn an_unmeasured_frame_refuses_on_width() {
        for degenerate in [0.0, -10.0, f32::NAN] {
            assert_eq!(
                refuse_another_pane(degenerate, 1),
                Some(PaneRefusal::TooNarrow { would_be: 2 })
            );
        }
    }

    /// The ceiling is asked before anything else, so a full layout never gets
    /// a sentence about width - not even on a frame that has not laid out.
    #[test]
    fn a_full_layout_is_the_ceiling_whatever_the_frame_says() {
        for width in [0.0, f32::NAN, 4000.0] {
            assert_eq!(
                refuse_another_pane(width, crate::layout::MAX_PANES),
                Some(PaneRefusal::Ceiling)
            );
        }
    }
}
