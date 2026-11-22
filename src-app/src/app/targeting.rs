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
//!    window's shape stable across dozens of switches. A pane showing an
//!    **opener** - a session whose sessions are still listed - is not
//!    taken here; see below.
//! 2. **An empty pane** - one with no surface in it, which in practice is a
//!    pane showing the launcher. Its whole purpose is to be filled.
//! 3. **A new pane**, if there are fewer than [`crate::layout::MAX_PANES`] and
//!    the split still leaves every pane at least [`MIN_PANE_FOR_SPLIT`] on the
//!    long axis.
//! 4. **The focused pane**, replaced - unless it shows an opener and another
//!    pane does not.
//!
//! **An opener is not the pane a switch lands in.** A coordinating session
//! is the agent pane, so rung 1 used to hand it every child the person
//! clicked: the session that was running the work went off the screen the
//! moment its work was looked at, and the person had to find it in the rail
//! to go back. So rungs 1 and 4 pass over a pane that shows an opener while
//! the ladder has anywhere else to go: a child lands in an empty pane, in a
//! new pane beside the opener when one fits, or replaces another pane. Only
//! when every pane shows an opener does the old answer stand. The hold needs
//! no setting and no pin: it lasts exactly as long as the opener has sessions
//! listed, which is what `opened_by` already records.
//!
//! **An opener's children share one pane.** A session another session opened
//! goes by [`place_child`], whichever door put it on screen (a click, the
//! palette, `splitlane add`, `splitlane show`): it becomes a tab of the pane
//! already showing one of its siblings, and only the first child takes a
//! pane - an empty one, a new one inserted beside the opener, or for a click
//! with no room the one the ladder above would replace. A coordinator with eight children then takes two
//! panes, not every pane on screen, and the other panes keep what they show.
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

/// What the ladder reads from one pane, in tree order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeafFacts {
    /// The kind of the surface the pane shows; `None` for an empty pane.
    pub kind: Option<SurfaceKind>,
    /// The pane shows an opener: a session with sessions it opened still
    /// listed in the project.
    pub opener: bool,
    /// The pane has the keyboard.
    pub focused: bool,
    /// The surface record the pane shows, when it shows one.
    pub shows: Option<u64>,
    /// The opener of the session the pane shows: the pane is that opener's
    /// **children pane**, where its other sessions gather as tabs.
    pub family: Option<u64>,
    /// How many surfaces the pane holds.
    pub tabs: usize,
}

/// The rung the ladder stopped on, with the pane's index in tree order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rung {
    SameKind(usize),
    Empty(usize),
    NewPane,
    Replace(usize),
    /// A session opened by another one, added as a tab to the pane already
    /// showing a session of the same opener.
    ChildrenTab(usize),
    /// A new pane inserted right after the pane showing the placed session's
    /// opener, so its children pane sits beside it.
    NewPaneBeside(usize),
}

impl Rung {
    /// The existing pane this rung lands in; `None` for the two that create
    /// one.
    fn index(self) -> Option<usize> {
        match self {
            Rung::SameKind(i) | Rung::Empty(i) | Rung::Replace(i) | Rung::ChildrenTab(i) => Some(i),
            Rung::NewPane | Rung::NewPaneBeside(_) => None,
        }
    }
}

/// The ladder as a decision over facts, so it can be tested without a
/// window. `room` is whether rung 3 may split.
///
/// A pane that is some opener's children pane is passed over by rung 1 and
/// is the last thing rung 4 replaces short of an opener: replacing it would
/// take every child it holds off the screen at once, for a session that is
/// not one of them.
pub(crate) fn climb(leaves: &[LeafFacts], kind: SurfaceKind, room: bool) -> Rung {
    if leaves.is_empty() {
        return Rung::NewPane;
    }
    if let Some(i) = leaves
        .iter()
        .position(|leaf| leaf.kind == Some(kind) && !leaf.opener && leaf.family.is_none())
    {
        return Rung::SameKind(i);
    }
    if let Some(i) = leaves.iter().position(|leaf| leaf.kind.is_none()) {
        return Rung::Empty(i);
    }
    if room {
        return Rung::NewPane;
    }
    let plain = |leaf: &LeafFacts| !leaf.opener && leaf.family.is_none();
    let focused = leaves.iter().position(|leaf| leaf.focused);
    let i = focused
        .filter(|&i| plain(&leaves[i]))
        .or_else(|| leaves.iter().position(plain))
        .or_else(|| focused.filter(|&i| !leaves[i].opener))
        .or_else(|| leaves.iter().position(|leaf| !leaf.opener))
        .or(focused)
        .unwrap_or(0);
    Rung::Replace(i)
}

/// Where a session `opener` opened goes, short of replacing anything: the
/// one rule every door that puts such a session on screen asks - a rail
/// click, the palette, `splitlane add`, `splitlane show`.
///
/// **An opener's children gather in one pane.** A coordinator hands out more
/// sessions than there are panes, and with every child taking a pane of its
/// own they spread over whatever else was on screen, one replacement at a
/// time. So a child joins the pane already showing one of its siblings, as a
/// tab; only when there is none does it take a pane - an empty one, or a new
/// one inserted beside the opener. A child of another opener never joins.
/// Nothing is stored: the pane is a children pane for as long as it shows a
/// child, and an ordinary pane again once it does not.
///
/// `None` when the only places left are panes showing something else. What
/// happens then is the asker's: a person's click replaces a pane
/// ([`climb_for`]), a session's open goes to the rail, since a session does
/// not take a person's pane.
pub(crate) fn place_child(leaves: &[LeafFacts], opener: u64, room: bool) -> Option<Rung> {
    if leaves.is_empty() {
        return Some(Rung::NewPane);
    }
    // The pane holding most of the siblings, the first of those on a tie: a
    // child a person put in a pane of its own does not split the group.
    let mut group: Option<usize> = None;
    for (i, leaf) in leaves.iter().enumerate() {
        if leaf.family == Some(opener) && group.is_none_or(|g| leaf.tabs > leaves[g].tabs) {
            group = Some(i);
        }
    }
    if let Some(i) = group {
        return Some(Rung::ChildrenTab(i));
    }
    if let Some(i) = leaves.iter().position(|leaf| leaf.kind.is_none()) {
        return Some(Rung::Empty(i));
    }
    if room {
        return Some(
            match leaves.iter().position(|leaf| leaf.shows == Some(opener)) {
                Some(i) => Rung::NewPaneBeside(i),
                None => Rung::NewPane,
            },
        );
    }
    None
}

/// The ladder for a person's click on a session `opened_by` opened, or the
/// ordinary one when `opened_by` is `None`: [`place_child`], and with no
/// place left there, the pane the ordinary ladder would replace - which then
/// shows a child and so becomes the children pane.
pub(crate) fn climb_for(
    leaves: &[LeafFacts],
    kind: SurfaceKind,
    room: bool,
    opened_by: Option<u64>,
) -> Rung {
    let Some(opener) = opened_by else {
        return climb(leaves, kind, room);
    };
    place_child(leaves, opener, room).unwrap_or_else(|| climb(leaves, kind, false))
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
        self.refuse_another_pane_in(self.active_idx, cx)
    }

    /// [`Self::refuse_another_pane_now`] for a container named by index, which
    /// need not be the one on screen: a session opened from a pane lands in
    /// that pane's own project, whichever project the window is showing. The
    /// panes area is the window's, and it is the same for every project.
    pub(crate) fn refuse_another_pane_in(&self, ws_idx: usize, cx: &App) -> Option<PaneRefusal> {
        let Some(root) = self.workspaces.get(ws_idx).and_then(|ws| ws.root.as_ref()) else {
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
        let (leaves, facts) = self.ladder_facts(ws_idx, cx)?;
        let rung = climb(&facts, kind, self.room_for_another_pane_now(cx));
        Some(match rung.index().and_then(|i| leaves.get(i)) {
            Some(pane) => PaneTarget::Existing(pane.clone()),
            None => PaneTarget::NewPane,
        })
    }

    /// The panes of `ws_idx` in tree order, and what the ladder reads from
    /// each. `None` only when the container does not exist. A container with
    /// no tree, or an empty one, answers with no panes, which the ladder
    /// turns into a first pane: "every surface is a pane's content" has no
    /// arm for a surface with nowhere to be.
    fn ladder_facts(&self, ws_idx: usize, cx: &App) -> Option<(Vec<Entity<Pane>>, Vec<LeafFacts>)> {
        let container = self.workspaces.get(ws_idx)?;
        let Some(root) = container.root.as_ref() else {
            return Some((Vec::new(), Vec::new()));
        };
        let leaves = root.collect_leaves();
        // `focused_pane_now` is the pair `⌃⇥` is answered from, written every
        // frame - so the ladder needs no `Window` and gives the same answer a
        // keystroke would. With no focused pane in this tree, the first pane
        // stands in for it, as it always has.
        let focused = self
            .focused_pane_now
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .filter(|pane| root.contains_leaf(pane))
            .or_else(|| leaves.first().cloned());
        let facts = leaves
            .iter()
            .map(|pane| {
                let shows = self.pane_thread(pane, cx);
                LeafFacts {
                    kind: self.pane_kind(pane, cx),
                    opener: shows.is_some_and(|id| self.is_opener_in(ws_idx, id)),
                    focused: focused.as_ref() == Some(pane),
                    shows,
                    family: shows.and_then(|id| self.opener_of(id)),
                    tabs: pane.read(cx).tabs.len(),
                }
            })
            .collect();
        Some((leaves, facts))
    }

    /// The opener whose children pane `pane` is: the opener of the session
    /// it shows, when another session opened it.
    pub(crate) fn pane_family(&self, pane: &Entity<Pane>, cx: &App) -> Option<u64> {
        self.pane_thread(pane, cx).and_then(|id| self.opener_of(id))
    }

    /// The surface record behind the terminal `pane` shows, if any.
    fn pane_thread(&self, pane: &Entity<Pane>, cx: &App) -> Option<u64> {
        let pane = pane.read(cx);
        pane.tabs
            .get(pane.selected_idx)?
            .as_terminal()?
            .read(cx)
            .agent_thread_id
    }

    /// The session that opened `thread_id`, when another session did.
    pub(crate) fn opener_of(&self, thread_id: u64) -> Option<u64> {
        self.thread_by_id(thread_id)?
            .opened_by
            .as_ref()
            .map(|by| by.id)
            .filter(|&by| by != thread_id)
    }

    /// Whether `thread_id` opened a session that is still listed in
    /// `ws_idx`.
    fn is_opener_in(&self, ws_idx: usize, thread_id: u64) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|container| {
            container.threads.iter().any(|thread| {
                thread.id != thread_id
                    && thread
                        .opened_by
                        .as_ref()
                        .is_some_and(|by| by.id == thread_id)
            })
        })
    }

    /// One line in the orchestration journal when the hold decided the
    /// placement: `spared` when the ladder without it would have replaced an
    /// opener, `replaced` when an opener was replaced anyway because every
    /// pane showed one. It is what tells, after a week, whether switching
    /// still takes the coordinator off the screen.
    fn journal_placement(
        &self,
        facts: &[LeafFacts],
        kind: SurfaceKind,
        room: bool,
        rung: Rung,
        placed_thread: Option<u64>,
    ) {
        let shown = rung.index().and_then(|i| facts.get(i));
        let replaced_opener = matches!(rung, Rung::SameKind(_) | Rung::Replace(_))
            && shown.is_some_and(|leaf| leaf.opener);
        let without_hold: Vec<LeafFacts> = facts
            .iter()
            .map(|leaf| LeafFacts {
                opener: false,
                family: None,
                ..*leaf
            })
            .collect();
        let before = climb(&without_hold, kind, room);
        let spared = !replaced_opener
            && matches!(before, Rung::SameKind(_) | Rung::Replace(_))
            && before
                .index()
                .and_then(|i| facts.get(i))
                .is_some_and(|leaf| leaf.opener);
        let outcome = match (replaced_opener, spared) {
            (true, _) => "replaced",
            (false, true) => "spared",
            (false, false) => return,
        };
        let session = placed_thread
            .map_or_else(crate::orchestration_journal::Session::default, |id| {
                self.journal_session(id)
            });
        crate::orchestration_journal::record(
            "opener_hold",
            session,
            serde_json::json!({
                "outcome": outcome,
                "rung": format!("{rung:?}"),
                "panes": facts.len(),
            }),
        );
    }

    /// One line in the orchestration journal for every child the ladder
    /// placed: whether it joined its opener's children pane as a tab, started
    /// one in a new pane, or turned a pane that was there into one. It is
    /// what tells whether children still spread over the screen.
    fn journal_children_pane(&self, rung: Rung, panes: usize, tabs: usize, thread_id: u64) {
        let step = match rung {
            Rung::ChildrenTab(_) => "joined",
            Rung::NewPaneBeside(_) => "beside",
            Rung::NewPane => "new_pane",
            Rung::Empty(_) | Rung::SameKind(_) | Rung::Replace(_) => "reused",
        };
        crate::orchestration_journal::record(
            "children_pane",
            self.journal_session(thread_id),
            serde_json::json!({
                "step": step,
                "rung": format!("{rung:?}"),
                "panes": panes,
                "tabs": tabs,
            }),
        );
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
        let placed_thread = match &tab {
            TabContent::Terminal(view) => view.read(cx).agent_thread_id,
            _ => None,
        };
        // Only an agent is gathered with its siblings: a session opens
        // agents, and a shell that happens to carry a record is not one of
        // its children.
        let opened_by = placed_thread
            .filter(|_| kind == SurfaceKind::Agent)
            .and_then(|id| self.opener_of(id));
        let (_, facts) = self.ladder_facts(ws_idx, cx)?;
        let room = self.room_for_another_pane_now(cx);
        let rung = climb_for(&facts, kind, room, opened_by);
        self.journal_placement(&facts, kind, room, rung, placed_thread);
        let pane = self.put_on_rung(ws_idx, rung, tab, None, true, cx)?;
        self.active_idx = ws_idx;
        self.pending_pane_focus = Some(pane.clone());
        self.reroot_files_tree(cx);
        self.save_session(cx);
        cx.notify();
        Some(pane)
    }

    /// [`place_child`] asked of `ws_idx`'s panes as they are now. `None` when
    /// the container does not exist or nothing but a replacement is left.
    pub(crate) fn child_rung(&self, ws_idx: usize, opener: u64, cx: &App) -> Option<Rung> {
        let (_, facts) = self.ladder_facts(ws_idx, cx)?;
        let room = self.refuse_another_pane_in(ws_idx, cx).is_none();
        place_child(&facts, opener, room)
    }

    /// Put `tab` where `rung` says. The one executor for every placement the
    /// ladder decided, whoever asked, so a rail click and `splitlane add`
    /// cannot come to put the same session in different places.
    ///
    /// `anchor` overrides the pane a new pane is inserted after (a caller
    /// that named a neighbour). `select` is whether a tab added to a children
    /// pane is raised: a person's click raises it, a session's open adds it
    /// behind the one the person is looking at. Focus is the caller's, except
    /// that raising a tab in the pane holding the keyboard keeps the keyboard
    /// in that pane rather than on a terminal that is no longer drawn.
    pub(crate) fn put_on_rung(
        &mut self,
        ws_idx: usize,
        rung: Rung,
        tab: TabContent,
        anchor: Option<Entity<Pane>>,
        select: bool,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Pane>> {
        let leaves = self
            .workspaces
            .get(ws_idx)?
            .root
            .as_ref()
            .map(|root| root.collect_leaves())
            .unwrap_or_default();
        let thread_id = match &tab {
            TabContent::Terminal(view) => view.read(cx).agent_thread_id,
            _ => None,
        };
        let pane = match rung {
            Rung::ChildrenTab(i) => {
                let pane = leaves.get(i)?.clone();
                // Added, not shown in place: the siblings already in the pane
                // stay one click away in its header.
                pane.update(cx, |pane, cx| pane.add_tab(tab, select, cx));
                let holds_keyboard = self
                    .focused_pane_now
                    .as_ref()
                    .and_then(|weak| weak.upgrade())
                    .is_some_and(|focused| focused == pane);
                if select && holds_keyboard {
                    self.pending_pane_focus = Some(pane.clone());
                }
                pane
            }
            Rung::NewPaneBeside(i) => {
                let anchor = anchor.or_else(|| leaves.get(i).cloned());
                self.append_pane_after(ws_idx, tab, anchor, cx)?
            }
            Rung::NewPane => self.append_pane_after(ws_idx, tab, anchor, cx)?,
            Rung::SameKind(i) | Rung::Empty(i) | Rung::Replace(i) => {
                // Shows, which replaces. Rungs 1, 2 and 4 all land here, and on
                // a laptop rung 4 is most of them - see the note above on why
                // that costs the user their place on screen and not their work.
                let pane = leaves.get(i)?.clone();
                pane.update(cx, |pane, cx| match tab {
                    TabContent::Terminal(view) => pane.show_terminal(view, cx),
                    other => pane.show_surface(other, cx),
                });
                pane
            }
        };
        if let Some(thread_id) = thread_id
            && self.opener_of(thread_id).is_some()
        {
            let tabs = pane.read(cx).tabs.len();
            self.journal_children_pane(rung, leaves.len(), tabs, thread_id);
        }
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
        self.append_pane_after(ws_idx, tab, None, cx)
    }

    /// [`Self::append_pane_with`], inserted right after `anchor` instead of
    /// at the end when one is named - the children pane beside its opener.
    /// The row is re-split evenly either way.
    fn append_pane_after(
        &mut self,
        ws_idx: usize,
        tab: TabContent,
        anchor: Option<Entity<Pane>>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Pane>> {
        let container = self.workspaces.get(ws_idx)?;
        let anchor = anchor
            .filter(|pane| {
                container
                    .root
                    .as_ref()
                    .is_some_and(|root| root.contains_leaf(pane))
            })
            .or_else(|| {
                container
                    .root
                    .as_ref()
                    .and_then(|root| root.collect_leaves().into_iter().next_back())
            });
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
        Some(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LeafFacts, MIN_PANE_FOR_SPLIT, PaneRefusal, Rung, SurfaceKind, climb, climb_for,
        place_child, refuse_another_pane,
    };

    fn agent(opener: bool, focused: bool) -> LeafFacts {
        LeafFacts {
            kind: Some(SurfaceKind::Agent),
            opener,
            focused,
            ..LeafFacts::default()
        }
    }

    fn shell(focused: bool) -> LeafFacts {
        LeafFacts {
            kind: Some(SurfaceKind::Shell),
            opener: false,
            focused,
            ..LeafFacts::default()
        }
    }

    /// A pane showing the opener `id`.
    fn coordinator(id: u64, focused: bool) -> LeafFacts {
        LeafFacts {
            shows: Some(id),
            ..agent(true, focused)
        }
    }

    /// A pane showing a session `opener` opened: that opener's children pane.
    fn child_of(opener: u64, focused: bool) -> LeafFacts {
        LeafFacts {
            family: Some(opener),
            ..agent(false, focused)
        }
    }

    /// The case that forced the hold: one pane, the coordinator in it, a
    /// child clicked in the rail. The child goes beside it when there is room.
    #[test]
    fn a_child_opens_beside_its_opener_when_a_pane_fits() {
        let panes = [agent(true, true)];
        assert_eq!(climb(&panes, SurfaceKind::Agent, true), Rung::NewPane);
    }

    /// With the coordinator and an agent pane on screen, the ordinary ladder
    /// takes the agent's pane, not the coordinator's - whichever has focus. (A
    /// child of that coordinator climbs `climb_for` and would be a tab.)
    #[test]
    fn the_next_child_replaces_the_child_not_the_opener() {
        let panes = [agent(true, true), agent(false, false)];
        assert_eq!(climb(&panes, SurfaceKind::Agent, false), Rung::SameKind(1));
        assert_eq!(climb(&panes, SurfaceKind::Agent, true), Rung::SameKind(1));
    }

    /// No room and the opener focused: another pane is replaced, even one of
    /// another kind, before the opener is.
    #[test]
    fn with_no_room_a_pane_that_is_not_an_opener_is_replaced() {
        let panes = [agent(true, true), shell(false)];
        assert_eq!(climb(&panes, SurfaceKind::Agent, false), Rung::Replace(1));
    }

    /// When every pane shows an opener the old answer stands: the focused
    /// pane is replaced, so a click is never refused.
    #[test]
    fn when_every_pane_is_an_opener_the_focused_one_is_replaced() {
        let panes = [agent(true, false), agent(true, true)];
        assert_eq!(climb(&panes, SurfaceKind::Agent, false), Rung::Replace(1));
        let alone = [agent(true, true)];
        assert_eq!(climb(&alone, SurfaceKind::Agent, false), Rung::Replace(0));
    }

    /// Without openers the ladder is the one it always was.
    #[test]
    fn without_an_opener_the_ladder_is_unchanged() {
        let panes = [agent(false, false), shell(true)];
        assert_eq!(climb(&panes, SurfaceKind::Agent, true), Rung::SameKind(0));
        assert_eq!(climb(&panes, SurfaceKind::Shell, true), Rung::SameKind(1));
        let empty = [agent(true, true), LeafFacts::default()];
        assert_eq!(climb(&empty, SurfaceKind::Agent, true), Rung::Empty(1));
        assert_eq!(climb(&[], SurfaceKind::Agent, false), Rung::NewPane);
    }

    /// The rule a session's open asks (`splitlane add`, `splitlane show`):
    /// the same places a click finds, and no replacement - with nothing but a
    /// replacement left the answer is `None`, and the session goes to the
    /// rail.
    #[test]
    fn a_session_opening_a_child_finds_the_places_a_click_finds() {
        let grouped = [coordinator(7, true), child_of(7, false)];
        assert_eq!(place_child(&grouped, 7, false), Some(Rung::ChildrenTab(1)));
        let alone = [coordinator(7, true)];
        assert_eq!(place_child(&alone, 7, true), Some(Rung::NewPaneBeside(0)));
        assert_eq!(place_child(&alone, 7, false), None);
        let beside_shell = [coordinator(7, true), shell(false)];
        assert_eq!(place_child(&beside_shell, 7, false), None);
        // A project with no panes takes the child as its first.
        assert_eq!(place_child(&[], 7, false), Some(Rung::NewPane));
        // Wherever `place_child` has an answer, a click gets the same one.
        for (panes, room) in [(&grouped[..], false), (&alone[..], true)] {
            assert_eq!(
                Some(climb_for(panes, SurfaceKind::Agent, room, Some(7))),
                place_child(panes, 7, room)
            );
        }
    }

    /// A child a person put in a pane of its own does not split the group:
    /// the next child joins the pane holding most of its siblings.
    #[test]
    fn the_next_child_joins_the_pane_holding_most_of_its_siblings() {
        let stray = child_of(7, false);
        let group = LeafFacts {
            tabs: 3,
            ..child_of(7, false)
        };
        let panes = [coordinator(7, true), stray, group];
        assert_eq!(place_child(&panes, 7, false), Some(Rung::ChildrenTab(2)));
    }

    /// A grandchild groups under the session that opened it, which is itself
    /// a tab of its own opener's children pane - no special case.
    #[test]
    fn a_grandchild_groups_under_its_immediate_opener() {
        // 7 coordinates; 8 is its child and has opened sessions itself.
        let middle = LeafFacts {
            shows: Some(8),
            opener: true,
            ..child_of(7, false)
        };
        let panes = [coordinator(7, false), middle];
        assert_eq!(place_child(&panes, 8, true), Some(Rung::NewPaneBeside(1)));
        let with_grandchild = [coordinator(7, false), middle, child_of(8, true)];
        assert_eq!(
            place_child(&with_grandchild, 8, false),
            Some(Rung::ChildrenTab(2))
        );
        assert_eq!(
            place_child(&with_grandchild, 7, false),
            Some(Rung::ChildrenTab(1))
        );
    }

    /// The first child of a coordinator alone on screen starts the children
    /// pane, inserted right after the coordinator's own.
    #[test]
    fn the_first_child_starts_a_children_pane_beside_its_opener() {
        let panes = [shell(false), coordinator(7, true)];
        assert_eq!(
            climb_for(&panes, SurfaceKind::Agent, true, Some(7)),
            Rung::NewPaneBeside(1)
        );
        // Room wins over a plain agent pane: the child does not take the
        // place of a session that has nothing to do with it.
        let beside_other = [coordinator(7, true), agent(false, false)];
        assert_eq!(
            climb_for(&beside_other, SurfaceKind::Agent, true, Some(7)),
            Rung::NewPaneBeside(0)
        );
        // An opener that is not on screen has nothing to sit beside.
        assert_eq!(
            climb_for(&[shell(true)], SurfaceKind::Agent, true, Some(7)),
            Rung::NewPane
        );
    }

    /// Every later child is a tab of that pane, whatever has focus and
    /// whether or not there is room for another pane - the coordinator is
    /// not touched.
    #[test]
    fn a_second_child_is_a_tab_in_the_same_pane() {
        let panes = [coordinator(7, true), child_of(7, false)];
        for room in [true, false] {
            assert_eq!(
                climb_for(&panes, SurfaceKind::Agent, room, Some(7)),
                Rung::ChildrenTab(1)
            );
        }
        let focused_child = [coordinator(7, false), child_of(7, true)];
        assert_eq!(
            climb_for(&focused_child, SurfaceKind::Agent, false, Some(7)),
            Rung::ChildrenTab(1)
        );
    }

    /// A child of another coordinator never joins this one's children pane,
    /// and does not replace it while a plain pane is there to take.
    #[test]
    fn a_child_of_another_opener_does_not_join() {
        let panes = [
            coordinator(7, false),
            child_of(7, true),
            coordinator(9, false),
        ];
        assert_eq!(
            climb_for(&panes, SurfaceKind::Agent, true, Some(9)),
            Rung::NewPaneBeside(2)
        );
        let with_shell = [coordinator(7, false), child_of(7, true), shell(false)];
        assert_eq!(
            climb_for(&with_shell, SurfaceKind::Agent, false, Some(9)),
            Rung::Replace(2)
        );
        // An ordinary session does not land in a children pane either.
        assert_eq!(
            climb(&with_shell, SurfaceKind::Agent, false),
            Rung::Replace(2)
        );
    }

    /// No room: the pane step 1 would replace becomes the children pane.
    #[test]
    fn with_no_room_the_pane_that_is_not_an_opener_becomes_the_children_pane() {
        let panes = [coordinator(7, true), shell(false)];
        assert_eq!(
            climb_for(&panes, SurfaceKind::Agent, false, Some(7)),
            Rung::Replace(1)
        );
        let agents = [coordinator(7, true), agent(false, false)];
        assert_eq!(
            climb_for(&agents, SurfaceKind::Agent, false, Some(7)),
            Rung::SameKind(1)
        );
        // An empty pane is taken before anything is replaced or added.
        let empty = [coordinator(7, true), LeafFacts::default()];
        assert_eq!(
            climb_for(&empty, SurfaceKind::Agent, true, Some(7)),
            Rung::Empty(1)
        );
    }

    /// A session nobody opened climbs the ordinary ladder.
    #[test]
    fn a_session_nobody_opened_climbs_the_ordinary_ladder() {
        let panes = [agent(false, false), shell(true)];
        for room in [true, false] {
            assert_eq!(
                climb_for(&panes, SurfaceKind::Agent, room, None),
                climb(&panes, SurfaceKind::Agent, room)
            );
        }
    }

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
