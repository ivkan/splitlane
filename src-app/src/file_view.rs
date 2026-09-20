//! `FileView` - GPUI entity that renders an owned `MdNode` AST.
//!
//! The view does no parsing of its own - `FileView::open` reads the file
//! from disk, runs `parser::parse_with_limit`, and stores the resulting AST.
//! `Render` walks the AST and emits a nested `div` element tree styled from
//! `MarkdownPalette` (which itself snapshots the active terminal theme).
//!
//! Live reload: `start_watcher` registers a `notify::RecommendedWatcher`
//! on the file's parent directory and spawns a `cx.spawn` task that debounces
//! events at 200 ms before re-reading the file and calling `cx.notify()`. The
//! watcher is owned by the entity, so closing the pane drops it and frees the
//! OS handle. Scroll position is preserved automatically: the GPUI element id
//! (`element_id`) is stable across re-renders.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use futures::future::Either;
use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Focusable, Font, FontFeatures, FontStyle,
    FontWeight, Hsla, InteractiveElement, IntoElement, KeyContext, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Point, Render, ScrollHandle, SharedString,
    StrikethroughStyle, Styled, StyledText, TextRun, UnderlineStyle, Window, div, point,
    prelude::*, px,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use pulldown_cmark::{Alignment, HeadingLevel};

use crate::markdown::parser::{MAX_INPUT_BYTES, MdNode, ParseError, Span, parse_with_limit};
use crate::markdown::state;
use crate::markdown::theme::MarkdownPalette;
use crate::ui_tokens as tok;

/// Debounce window for the live-reload watcher. Many editors and
/// AI agents stream writes - a single user-perceived save fires multiple
/// `Modify` events within ~50 ms. 200 ms is the sweet spot: long
/// enough to coalesce a streaming write, short enough to feel instant.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);

/// Approximate scroll page-step in CSS pixels. Used by `MarkdownScrollPageUp`
/// / `MarkdownScrollPageDown` when we don't know the precise viewport
/// height - close enough to one screen for typical terminal-pane sizes.
const PAGE_SCROLL_PX: f32 = 480.0;

/// Throttle window for scroll-position persistence writes. Scrolling fires
/// many GPUI ticks per second; we coalesce within this window to avoid a
/// disk write per pixel.
const SCROLL_PERSIST_THROTTLE: Duration = Duration::from_millis(750);

/// Polling cadence for the persistence task - checks the scroll handle's
/// current offset, writes if it changed and the throttle has elapsed.
const SCROLL_POLL_CADENCE: Duration = Duration::from_millis(250);

/// Cap on the byte size of clipboard payloads produced by `MarkdownCopy`.
/// Larger documents are truncated with a trailing ellipsis. Most platform
/// clipboards (NSPasteboard, X11 selections, Win32) accept multi-MB payloads
/// fine, but a 10 MB markdown copied to the clipboard is almost certainly
/// not what the user wanted - search-match copies are the common path.
const COPY_MAX_BYTES: usize = 64 * 1024;

const RENDER_PATH_ROOT: u64 = 14_695_981_039_346_656_037;
const MAX_RENDERED_TABLE_COLUMNS: u16 = 64;

/// Measure of the reading column, per the design's "Body max-width 620".
/// Composition rather than typography, so it is a local constant and not a
/// `ui_tokens` entry (see that module's "What is deliberately not in here").
/// A document read at pane width becomes a 200-character line, which is the
/// one thing a rendered document is supposed to fix about a raw one.
const READING_COLUMN_WIDTH: f32 = 620.0;

static MARKDOWN_VIEW_ID: AtomicU64 = AtomicU64::new(1);

/// A markdown viewer pane. One instance per opened file.
pub struct FileView {
    /// Absolute path to the file on disk. Stored for display in the title bar
    /// and consumed by the live-reload watcher.
    pub path: PathBuf,
    /// What this surface found in the file. One value: a file is one of these
    /// things, and the two `Option` fields this replaced could hold "a
    /// document *and* an error" and had to be read in the right order to be
    /// correct.
    body: FileBody,
    focus_handle: FocusHandle,
    /// Which theme the syntax colours in [`TextBody::runs`] were resolved
    /// against.
    ///
    /// The runs carry `Hsla`, not capture names, so a theme change makes them
    /// stale - and the fix is not to re-read the file (that would throw away
    /// the reader's selection for a reason that has nothing to do with the
    /// file) but to re-colour the text already in hand. `u64::MAX` means "no
    /// colours yet", which no real generation can be.
    syntax_generation: u64,
    /// Stable GPUI element id, computed once at construction so the render
    /// hot path doesn't re-`format!` the path on every frame.
    element_id: SharedString,
    /// Owned watcher handle. `Some` when the live-reload pipeline is
    /// active. Dropping the entity drops this field, which unregisters the OS
    /// watch and closes the channel sender; the spawned debounce loop sees
    /// the closed channel on its next `next().await` and terminates.
    _watcher: Option<RecommendedWatcher>,
    /// Scroll handle attached to the viewer's outer scroll container.
    /// Owned here so action handlers (`MarkdownScrollPageUp/Down`) and the
    /// persistence task can read/write the offset.
    scroll_handle: ScrollHandle,
    /// Vertical offset to restore on next render. `Some` until the
    /// pending value is applied to the scroll handle (handled by a one-shot
    /// task that fires after the first paint computes `max_offset`). Storing
    /// raw f32 (CSS pixels) keeps the on-disk format simple.
    pending_restore_y: Option<f32>,
    /// Search overlay state. `search_active` gates the bar visibility
    /// and the `MarkdownSearch` key context that captures Enter/Esc/typing.
    search_active: bool,
    search_query: String,
    /// The find bar's `Aa` toggle, matching the terminal's. Off by default.
    search_case_sensitive: bool,
    /// Plain-text snapshot of the rendered AST, lazily rebuilt when the AST
    /// changes. Searching this string is O(n) per query - fine for files up
    /// to `MAX_INPUT_BYTES`.
    search_corpus: String,
    /// Lowercased search corpus plus a byte-offset map back to `search_corpus`.
    /// Kept only while search is active so normal reading does not pay for it.
    search_corpus_lower: String,
    search_lower_to_source: Vec<usize>,
    /// Byte offsets of each match in `search_corpus`. Empty when no query is
    /// set or no matches exist.
    search_matches: Vec<usize>,
    /// Index into `search_matches` for the currently focused match.
    search_current: usize,
    /// Drag-to-scroll state for the visible scrollbar overlay. `None` when
    /// the user isn't currently dragging the thumb.
    scroll_drag: Option<crate::widgets::scrollbar::ScrollDragState>,
    /// The lines a person has picked out of a printed file, as
    /// `(anchor, focus)` **inclusive** and in either order - the anchor is
    /// where the gesture started, so a selection dragged upwards has the
    /// larger index first. `None` when nothing is picked.
    ///
    /// # Lines, and not characters
    ///
    /// This surface draws one `div` per line and hands each one a
    /// `SharedString`; GPUI shapes that text inside the div and exposes no
    /// character offset for a point inside a shaped `div` - which is true of a
    /// `div` and **false of `StyledText`**, whose `TextLayout` answers
    /// `index_for_position` and `position_for_index`. That is what we
    /// found, and it is the whole reason this is a character selection and not
    /// a line one: the lines became `StyledText` for syntax colouring, and the
    /// hit-testing came with them. No custom `Element`, no borrowed editor.
    ///
    /// A point is `(line, byte)`. The pair is (anchor, head) in the order the
    /// gesture made them, so a drag upwards is stored head-before-anchor and
    /// `picked_range` is the one place that sorts.
    ///
    /// What it still costs, stated rather than hidden: nothing at all can be
    /// taken out of a **rendered** document, which has no lines to point at.
    /// `⌘C` with nothing picked still copies the whole file, which is what it
    /// always did.
    text_selection: Option<(TextPoint, TextPoint)>,
    /// How many times a body has been applied to this view.
    ///
    /// Only the theme recolour reads it, and only to answer one question: is
    /// the text I just parsed still the text on screen? Comparing line counts
    /// was the first guard and is not enough - an agent editing a file in place
    /// very often leaves the count alone.
    load_seq: u64,
    /// This frame's text layouts, one per printed line, in line order.
    ///
    /// Rebuilt every render and filled by GPUI during prepaint. A mouse event
    /// is dispatched after a paint, so by the time a handler reads this the
    /// entries are measured - and a handler only ever asks about the row that
    /// received the event, which is by definition a row that painted.
    line_layouts: Vec<gpui::TextLayout>,
    /// Whether the pointer is down and dragging a selection out.
    ///
    /// Its only job is to gate the per-line `on_mouse_move` listener: a file
    /// draws up to [`MAX_TEXT_LINES`] rows and hanging a move listener on
    /// every one of them for the whole time the pane is open would be two
    /// thousand closures rebuilt every frame for a gesture that is not
    /// happening. While this is false the rows carry a mouse-down listener and
    /// nothing else.
    text_dragging: bool,
}

impl FileView {
    /// Read `path` from disk and build a view. IO errors are surfaced via the
    /// `error` field; the view is still created so the user sees the message
    /// instead of the click silently failing.
    ///
    /// On a successful first read, registers a `notify` watcher on
    /// the file's parent directory and spawns the debounce/reload loop.
    pub fn open(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let element_id = make_element_id(&path);
        // Restore last-known scroll offset for this file (if any).
        // Goes through the shared state mutex so concurrent panes never
        // observe a half-written cache.
        let pending_restore_y = state::lookup_offset_for(&path);
        let view = Self {
            // No colours yet - the first load resolves them against whatever
            // theme is active when it lands.
            syntax_generation: u64::MAX,
            path,
            body: FileBody::Error("Loading\u{2026}".into()),
            focus_handle: cx.focus_handle(),
            element_id,
            _watcher: None,
            scroll_handle: ScrollHandle::new(),
            pending_restore_y,
            search_active: false,
            search_query: String::new(),
            search_case_sensitive: false,
            search_corpus: String::new(),
            search_corpus_lower: String::new(),
            search_lower_to_source: Vec::new(),
            search_matches: Vec::new(),
            search_current: 0,
            scroll_drag: None,
            text_selection: None,
            load_seq: 0,
            line_layouts: Vec::new(),
            text_dragging: false,
        };
        view.start_initial_load(cx);
        view.start_scroll_persistence(cx);
        view
    }

    fn start_initial_load(&self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        // Snapshotted here, on the render thread: `active_theme()` polls an
        // mtime, which is the one thing a background read must not do. Same
        // move, same reason, as `diff/view/loader.rs`.
        let (syntax, generation) = syntax_snapshot();
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let body = smol::unblock(move || load_from_disk(&path, Some(syntax))).await;
                cx.update(|cx| {
                    let _ = this.update(cx, |view: &mut Self, cx: &mut Context<Self>| {
                        view.syntax_generation = generation;
                        view.apply_loaded(body);
                        // Start watching after the first snapshot is applied
                        // so a slow initial read cannot overwrite a newer
                        // watcher reload that raced ahead of it.
                        view.start_watcher(cx);
                        view.maybe_apply_pending_restore(cx);
                        cx.notify();
                    });
                });
            },
        )
        .detach();
    }

    /// Re-run the highlighter over the text already loaded, if the theme moved.
    ///
    /// Deliberately not a reload: the bytes have not changed, only what colour
    /// a keyword is. The lines are rejoined rather than kept as a second copy
    /// of the file, because a `String` built once per theme change is cheaper
    /// than one held for the life of every open file.
    fn recolor_if_theme_changed(&mut self, cx: &mut Context<Self>) {
        let generation = crate::theme::theme_generation();
        if generation == self.syntax_generation {
            return;
        }
        let FileBody::Text(text) = &self.body else {
            // Non-text bodies still adopt the generation below so the question
            // is not asked again every frame for the life of the surface.
            // Nothing to recolour - and the generation is still adopted, so a
            // markdown or opaque file does not ask this question again every
            // frame for the rest of its life.
            self.syntax_generation = generation;
            return;
        };
        self.syntax_generation = generation;
        // A file with no grammar, or one whose grammar found nothing, has no
        // colours to change - and re-parsing it on every theme change buys a
        // guaranteed-empty answer. The same condition covers both, because a
        // file with no grammar is exactly a file whose runs are all empty.
        if text.runs.iter().all(Vec::is_empty) {
            return;
        }
        let joined = text
            .lines
            .iter()
            .map(SharedString::as_ref)
            .collect::<Vec<_>>()
            .join("\n");
        let ext = text.ext.clone();
        let shown = text.lines.len();
        let (syntax, spawned_generation) = syntax_snapshot();
        let seq = self.load_seq;
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let runs =
                    smol::unblock(move || highlight_for(&joined, &ext, Some(syntax), shown)).await;
                cx.update(|cx| {
                    let _ = this.update(cx, |view: &mut Self, cx: &mut Context<Self>| {
                        // The file may have reloaded while this ran, and a reload
                        // brings its own colours. `load_seq` is the check, not
                        // the line count: a file edited in place usually keeps
                        // its count, so counting alone would let a parse of the
                        // old text repaint the new one and stay wrong until the
                        // next reload.
                        // Two guards, because two different things can move
                        // while a parse runs. `load_seq` is the text: a reload
                        // brings its own colours, and a file edited in place
                        // usually keeps its line count, so counting alone would
                        // let a parse of the old bytes repaint the new ones.
                        // `syntax_generation` is the palette: two theme changes
                        // in quick succession spawn twice, and the older parse
                        // can finish last - without this it would win, and no
                        // later frame would repair it, because the generation
                        // already says the newer theme has been applied.
                        if view.load_seq == seq
                            && view.syntax_generation == spawned_generation
                            && let FileBody::Text(text) = &mut view.body
                            && text.lines.len() == runs.len()
                        {
                            text.runs = runs;
                            cx.notify();
                        }
                    });
                });
            },
        )
        .detach();
    }

    /// Whether "open this" means an editor or the OS.
    ///
    /// A `.png` has no editor to be opened in, and offering one would be a row
    /// that names the wrong thing; a `package.json` handed to the OS opens in
    /// whatever claimed the extension, which is not what the person choosing
    /// an editor in Settings asked for. One row either way, and this is which.
    pub(crate) fn opens_in_an_editor(&self) -> bool {
        matches!(self.body, FileBody::Markdown(_) | FileBody::Text(_))
    }

    fn apply_loaded(&mut self, body: FileBody) {
        self.body = body;
        // Line 40 of the old file is not line 40 of the new one. A selection
        // kept across a reload would highlight whatever moved into those rows
        // and hand it to the clipboard on the next `⌘C`, which is the one way
        // this feature could give somebody text they never picked. The watcher
        // fires on every save an agent makes, so this is the ordinary path.
        self.text_selection = None;
        self.text_dragging = false;
        // The layouts belong to the frame that painted the old lines. Nothing
        // asks an entry it did not just paint, so this is not what keeps the
        // hit test safe - it is what stops a body that stops being text (an
        // agent replacing a file with a binary, a read that now fails) from
        // leaving a measured layout behind for a surface that draws no lines.
        self.line_layouts.clear();
        // Every reload is a new text, and a colour run computed against the old
        // one describes bytes that have moved.
        self.load_seq = self.load_seq.wrapping_add(1);
        // Only refresh the search corpus when the find bar is open
        // Live-reload fires every 200 ms during streaming
        // writes; rebuilding a multi-MB corpus on every tick would be wasted
        // work for the common case where the user is just reading.
        if self.search_active {
            self.search_corpus = self.body.corpus();
            let (lower, map) = lowercase_with_byte_map(&self.search_corpus);
            self.search_corpus_lower = lower;
            self.search_lower_to_source = map;
            self.recompute_matches();
        } else {
            self.search_corpus.clear();
            self.search_corpus_lower.clear();
            self.search_lower_to_source.clear();
            self.search_matches.clear();
            self.search_current = 0;
        }
    }

    /// Rebuild `search_matches` from the current `search_query` and corpus.
    /// Called on query change, on AST reload, and when the bar opens.
    fn recompute_matches(&mut self) {
        self.search_matches.clear();
        if self.search_query.is_empty() {
            self.search_current = 0;
            return;
        }
        // The `Aa` toggle: with it on the source corpus is the haystack and a
        // hit's byte offset is already a source offset, so the lowercase map is
        // simply not consulted. Keeping one loop rather than two is what stops
        // the two modes from drifting apart on the next edit.
        let case_sensitive = self.search_case_sensitive;
        let needle = if case_sensitive {
            self.search_query.clone()
        } else {
            self.search_query.to_lowercase()
        };
        let haystack = if case_sensitive {
            &self.search_corpus
        } else {
            &self.search_corpus_lower
        };
        let mut start = 0;
        while let Some(pos) = haystack[start..].find(&needle) {
            let abs = start + pos;
            let source_abs = if case_sensitive {
                Some(abs)
            } else {
                self.search_lower_to_source.get(abs).copied()
            };
            if let Some(source_abs) = source_abs {
                self.search_matches.push(source_abs);
            }
            start = abs + needle.len().max(1);
        }
        if !self.search_matches.is_empty() {
            self.search_current = self.search_current.min(self.search_matches.len() - 1);
        } else {
            self.search_current = 0;
        }
    }

    /// Proportional scroll-to-match. We don't have per-span pixel
    /// offsets in the AST, so we approximate by mapping the byte offset in
    /// the search corpus to a fraction of `max_offset`. Coarse but useful:
    /// the user lands close enough to the match to spot it visually.
    fn scroll_to_current_match(&self) {
        let Some(byte_offset) = self.search_matches.get(self.search_current).copied() else {
            return;
        };
        let total = self.search_corpus.len();
        if total == 0 {
            return;
        }
        let fraction = byte_offset as f32 / total as f32;
        let max = self.scroll_handle.max_offset();
        // Offset is negative-down per ScrollHandle convention.
        let target = max.y * fraction;
        self.scroll_handle.set_offset(point(px(0.0), -target));
    }

    /// Schedule the pending scroll restore once the document has
    /// painted at least once. `set_offset` itself does not clamp, but until
    /// the scroll container has been laid out the `ScrollHandle` is not yet
    /// linked to the layout node (`track_scroll` only takes effect during
    /// prepaint). Setting an offset before the first paint writes to a
    /// detached handle and is functionally a no-op. Sleeping one tick lets
    /// the first paint complete; afterwards the handle drives layout.
    /// Non-finite values (NaN/Inf) from a hand-edited cache are dropped.
    fn maybe_apply_pending_restore(&self, cx: &mut Context<Self>) {
        if self.pending_restore_y.is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            smol::Timer::after(Duration::from_millis(80)).await;
            cx.update(|cx| {
                let _ = this.update(cx, |view: &mut Self, _cx| {
                    if let Some(y) = view.pending_restore_y.take()
                        && y.is_finite()
                    {
                        view.scroll_handle.set_offset(point(px(0.0), px(-y)));
                    }
                });
            });
        })
        .detach();
    }

    /// Long-running task that persists the scroll offset to the JSON
    /// cache as the user scrolls. Polls every `SCROLL_POLL_CADENCE`; flushes
    /// to disk when the offset has changed by more than 1 px AND the throttle
    /// window has elapsed since the last write. The task self-terminates on
    /// entity drop via the standard `WeakEntity` cancellation.
    ///
    /// Writes go through `state::save_offset_for` which serialises through a
    /// process-wide mutex - concurrent persistence tasks from multiple
    /// markdown panes therefore never lose updates from each other.
    fn start_scroll_persistence(&self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let handle = self.scroll_handle.clone();
        cx.spawn(async move |this: gpui::WeakEntity<Self>, _cx| {
            let mut last_persisted: f32 = f32::from(handle.offset().y);
            let mut last_write = Instant::now();
            loop {
                smol::Timer::after(SCROLL_POLL_CADENCE).await;
                if this.upgrade().is_none() {
                    let current: f32 = f32::from(handle.offset().y);
                    if (current - last_persisted).abs() >= 1.0
                        && let Err(e) = state::save_offset_for(&path, -current)
                    {
                        log::warn!("markdown_state.json final save failed: {}", e);
                    }
                    break;
                }
                let current: f32 = f32::from(handle.offset().y);
                if (current - last_persisted).abs() < 1.0 {
                    continue;
                }
                if last_write.elapsed() < SCROLL_PERSIST_THROTTLE {
                    continue;
                }
                // Store as positive vertical offset for clarity in the JSON.
                if let Err(e) = state::save_offset_for(&path, -current) {
                    log::warn!("markdown_state.json save failed: {}", e);
                }
                last_persisted = current;
                last_write = Instant::now();
            }
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Action handlers
    // ---------------------------------------------------------------------

    fn handle_scroll_page_up(
        &mut self,
        _: &crate::MarkdownScrollPageUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cur = self.scroll_handle.offset();
        // Less-negative y = scrolled up.
        self.scroll_handle
            .set_offset(point(cur.x, (cur.y + px(PAGE_SCROLL_PX)).min(px(0.0))));
        cx.notify();
    }

    fn handle_scroll_page_down(
        &mut self,
        _: &crate::MarkdownScrollPageDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cur = self.scroll_handle.offset();
        let max = self.scroll_handle.max_offset();
        // Bottom of content corresponds to `-max.y`. Clamp so we don't
        // over-scroll past it.
        let target_y = (cur.y - px(PAGE_SCROLL_PX)).max(-max.y);
        self.scroll_handle.set_offset(point(cur.x, target_y));
        cx.notify();
    }

    fn handle_find_open(
        &mut self,
        _: &crate::MarkdownFindOpen,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_active = true;
        // Build the corpus on demand the first time the bar opens.
        self.search_corpus = self.body.corpus();
        if self.search_corpus.is_empty() {
            self.search_corpus_lower.clear();
            self.search_lower_to_source.clear();
        } else {
            let (lower, map) = lowercase_with_byte_map(&self.search_corpus);
            self.search_corpus_lower = lower;
            self.search_lower_to_source = map;
        }
        self.recompute_matches();
        cx.notify();
    }

    fn handle_find_dismiss(
        &mut self,
        _: &crate::MarkdownFindDismiss,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_active = false;
        self.search_query.clear();
        self.search_corpus.clear();
        self.search_corpus_lower.clear();
        self.search_lower_to_source.clear();
        self.search_matches.clear();
        self.search_current = 0;
        cx.notify();
    }

    fn handle_find_next(
        &mut self,
        _: &crate::MarkdownFindNext,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_current = (self.search_current + 1) % self.search_matches.len();
        self.scroll_to_current_match();
        cx.notify();
    }

    fn handle_find_prev(
        &mut self,
        _: &crate::MarkdownFindPrev,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_matches.is_empty() {
            return;
        }
        let len = self.search_matches.len();
        self.search_current = (self.search_current + len - 1) % len;
        self.scroll_to_current_match();
        cx.notify();
    }

    /// Copy support. GPUI in the pinned commit does not expose
    /// drag-text-selection across `div(...).child(SharedString)` trees. As a
    /// pragmatic substitute we copy either the active search match (with
    /// surrounding context) or the entire flat text. Mouse drag-selection
    /// over rendered markdown is a documented follow-up gap.
    fn handle_copy(&mut self, _: &crate::MarkdownCopy, _: &mut Window, cx: &mut Context<Self>) {
        let payload = if let Some(lines) = self.selected_text() {
            // A selection is the most specific thing the person has said about
            // what they want, so it outranks both of the fallbacks below.
            lines
        } else if self.search_active && !self.search_matches.is_empty() {
            self.context_around_match()
        } else {
            // Build the flat text on demand - the corpus field is only kept
            // up to date when the find bar is active.
            self.body.corpus()
        };
        if payload.is_empty() {
            return;
        }
        let bounded = truncate_for_clipboard(&payload);
        cx.write_to_clipboard(ClipboardItem::new_string(bounded));
    }

    /// The picked lines of a printed file, joined, with no numbers.
    ///
    /// The gutter is this surface's own furniture and pasting it into a shell
    /// or an agent would break whatever the text is - which is the whole reason
    /// somebody is copying it.
    fn selected_text(&self) -> Option<String> {
        let (anchor, head) = self.text_selection?;
        let FileBody::Text(text) = &self.body else {
            return None;
        };
        let (lo, hi) = ordered(anchor, head);
        if lo == hi {
            // A click that picked nothing is not a selection. Falling through
            // to the whole file is what `⌘C` does with no pick at all, and a
            // caret is no pick.
            return None;
        }
        let mut out = String::new();
        for line_idx in lo.0..=hi.0.min(text.lines.len().saturating_sub(1)) {
            let line = text.lines.get(line_idx)?.as_ref();
            let start = if line_idx == lo.0 { lo.1 } else { 0 };
            let end = if line_idx == hi.0 { hi.1 } else { line.len() };
            let (start, end) = clamp_to_line(line, start, end);
            if line_idx > lo.0 {
                out.push('\n');
            }
            out.push_str(&line[start..end]);
        }
        // Two ends that are not equal can still enclose no bytes - a stale
        // selection whose offsets both clamp to the end of a line that shrank.
        // Handing `Some("")` to the copy path would clear the clipboard while
        // reporting a successful copy, instead of falling through to the whole
        // file the way an unpicked `\u{2318}C` does.
        (!out.is_empty()).then_some(out)
    }

    /// The word under `point`, as a selection.
    ///
    /// Word in the Unicode sense (`unicode_word_indices`), which is what a
    /// double click means in every editor and what makes it useful on code: it
    /// takes `session_id` whole and stops at the dot in `foo.bar`. A double
    /// click that lands on whitespace or punctuation selects nothing rather
    /// than guessing at a neighbour.
    fn word_at(&self, point: TextPoint) -> Option<(TextPoint, TextPoint)> {
        let FileBody::Text(text) = &self.body else {
            return None;
        };
        let line = text.lines.get(point.0)?.as_ref();
        let (start, end) = word_bounds(line, point.1)?;
        Some(((point.0, start), (point.0, end)))
    }

    /// The whole of line `idx`, as a selection - what a third click means.
    fn line_at(&self, idx: usize) -> Option<(TextPoint, TextPoint)> {
        let FileBody::Text(text) = &self.body else {
            return None;
        };
        let line = text.lines.get(idx)?;
        Some(((idx, 0), (idx, line.len())))
    }

    /// Where in the file a pointer landed, or `None` when this line was not
    /// painted this frame.
    ///
    /// The layout is the one the row's own `StyledText` filled during prepaint,
    /// so asking it is only safe for a row that painted - which is exactly the
    /// row that received the event, since a row nothing drew receives nothing.
    fn hit(&self, line: usize, position: gpui::Point<gpui::Pixels>) -> Option<TextPoint> {
        let layout = self.line_layouts.get(line)?;
        // `Err` is the nearest index rather than a failure: past the end of a
        // line it is the end, which is what dragging off the right edge should
        // mean.
        let byte = layout
            .index_for_position(position)
            .unwrap_or_else(|nearest| nearest);
        Some((line, byte))
    }

    /// End a drag, and rebuild the frame without the per-line move listeners.
    fn end_text_drag(&mut self, cx: &mut Context<Self>) {
        if self.text_dragging {
            self.text_dragging = false;
            cx.notify();
        }
    }

    /// Extract the line of `search_corpus` containing the current match,
    /// trimmed to a reasonable preview length. Used by `handle_copy`.
    fn context_around_match(&self) -> String {
        let Some(&offset) = self.search_matches.get(self.search_current) else {
            return String::new();
        };
        let bytes = self.search_corpus.as_bytes();
        let mut start = offset;
        while start > 0 && bytes[start - 1] != b'\n' {
            start -= 1;
        }
        let mut end = offset;
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        self.search_corpus[start..end].to_string()
    }

    /// Handle keystrokes routed via the `MarkdownSearch` key context
    /// when the find bar is open. Printable ASCII chars append to the query;
    /// Backspace removes the last char. Arrow keys / Enter / Esc are handled
    /// by their respective bound actions.
    fn handle_search_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.search_active {
            return;
        }
        let key = &event.keystroke.key;
        match key.as_str() {
            "backspace" => {
                if self.search_query.pop().is_some() {
                    self.recompute_matches();
                    self.scroll_to_current_match();
                    cx.notify();
                }
            }
            _ => {
                if let Some(ime_key) = event.keystroke.key_char.as_deref()
                    && !ime_key.is_empty()
                    && ime_key.chars().all(|c| !c.is_control())
                {
                    self.search_query.push_str(ime_key);
                    self.recompute_matches();
                    self.scroll_to_current_match();
                    cx.notify();
                }
            }
        }
    }

    /// Install the file watcher and spawn the debounce loop.
    ///
    /// We watch the *parent directory* non-recursively (matching
    /// `ConfigWatcher` / `ThemeWatcher`) so atomic-save patterns
    /// (`write to tmp + rename over original`) are caught: the file being
    /// removed and recreated would defeat a watch on the file inode itself.
    /// Events for siblings are filtered out by file_name match.
    fn start_watcher(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.path.parent().map(|p| p.to_path_buf()) else {
            log::warn!(
                "markdown watcher: path {} has no parent directory; live reload disabled",
                self.path.display()
            );
            return;
        };
        if !parent.exists() {
            log::warn!(
                "markdown watcher: parent dir {} does not exist; live reload disabled",
                parent.display()
            );
            return;
        }
        let target_filename = match self.path.file_name() {
            Some(name) => name.to_os_string(),
            None => {
                log::warn!(
                    "markdown watcher: path {} has no file name; live reload disabled",
                    self.path.display()
                );
                return;
            }
        };

        // `mpsc::unbounded` is the only async-friendly channel that supports
        // sync `unbounded_send` from the notify OS thread without blocking.
        // Critical invariant: events delivered between this line and the
        // first `rx.next().await` in the spawned task below are *queued*,
        // not lost - `unbounded` has no capacity limit. Switching to a
        // bounded channel without revisiting this race window would silently
        // drop the very-first event after `start_watcher` returns.
        let (tx, mut rx) = mpsc::unbounded::<notify::Result<notify::Event>>();
        let mut watcher = match RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let _ = tx.unbounded_send(res);
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("markdown watcher: failed to create watcher: {}", e);
                return;
            }
        };
        if let Err(e) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
            log::warn!(
                "markdown watcher: failed to watch {}: {}",
                parent.display(),
                e
            );
            return;
        }
        // Keep the watcher alive on the entity. Dropping the entity drops the
        // watcher, which unregisters the OS handle and closes the channel.
        self._watcher = Some(watcher);
        let path = self.path.clone();

        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                // Outer loop: each iteration consumes one debounced burst.
                // `rx.next().await == None` ⇒ channel closed (entity /
                // watcher dropped) ⇒ exit cleanly.
                while let Some(first) = rx.next().await {
                    if !event_is_relevant(&first, &target_filename) {
                        continue;
                    }
                    // Coalesce subsequent events that arrive within the debounce
                    // window. We re-read once after the burst settles.
                    let deadline = Instant::now() + RELOAD_DEBOUNCE;
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        let timer = smol::Timer::after(remaining);
                        match futures::future::select(rx.next(), timer).await {
                            Either::Left((Some(res), _)) => {
                                let _ = res; // event already accounted for; we re-read once at end
                            }
                            Either::Left((None, _)) => return,
                            Either::Right(_) => break,
                        }
                    }
                    let path = path.clone();
                    // Re-snapshotted per reload rather than captured once: the
                    // theme can change while a file sits open, and a reload
                    // must not repaint it in the old palette.
                    let (syntax, generation) = cx.update(|_| syntax_snapshot());
                    let body = smol::unblock(move || load_from_disk(&path, Some(syntax))).await;

                    // Apply the parsed snapshot on the GPUI main thread.
                    // `is_err()` catches the AsyncApp-dropped case directly. The
                    // entity-dropped case (this.update returning Err) is
                    // handled by the natural channel-closure chain: when
                    // FileView is dropped, `_watcher` drops, the notify
                    // sender drops, `rx.next().await` returns None, and the
                    // outer `while let` exits on the next iteration.
                    if cx
                        .update(|cx| {
                            this.update(cx, |view: &mut Self, cx: &mut Context<Self>| {
                                view.syntax_generation = generation;
                                view.apply_loaded(body);
                                view.maybe_apply_pending_restore(cx);
                                cx.notify();
                            })
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            },
        )
        .detach();
    }

    /// User-facing display title. Matches the file's basename so the pane's
    /// tab strip shows e.g. `README.md` rather than the absolute path.
    pub fn title(&self) -> SharedString {
        let owned: String = match self.path.file_name().and_then(|s| s.to_str()) {
            Some(name) => name.to_string(),
            None => self.path.to_string_lossy().into_owned(),
        };
        SharedString::from(owned)
    }

    /// What this surface says about itself beside its name.
    ///
    /// A rendered document keeps the design's `markdown \u{b7} read only`, which is
    /// the whole answer to "why is there no composer here". A text file says
    /// its extension, uppercased - the shortest true thing to put on a mono
    /// pane, which unlike a rendered one does not announce what it is by
    /// looking like it - and keeps the read-only half, because "Splitlane never
    /// edits files" is a promise and it belongs on every file surface. A file
    /// that was cut says so there rather than only at the foot of the body,
    /// where a reader who never scrolls would not find it.
    pub fn badge(&self) -> Option<SharedString> {
        match &self.body {
            FileBody::Markdown(_) => Some(SharedString::from("markdown \u{b7} read only")),
            FileBody::Text(text) => {
                let kind = extension_badge(&self.path).unwrap_or_else(|| "text".into());
                Some(SharedString::from(match text.cut_after {
                    Some(cut) => format!("{kind} \u{b7} first {cut} lines"),
                    None => format!("{kind} \u{b7} read only"),
                }))
            }
            FileBody::Opaque { kind, .. } => {
                Some(SharedString::from(format!("{kind} \u{b7} not text")))
            }
            FileBody::Error(_) => None,
        }
    }

    /// The one word a rail row puts beside this surface's name.
    ///
    /// Before the first read lands the body is an error saying "Loading", and
    /// the file's own name is the better answer: a `.md` row that reads `text`
    /// for one frame and flips to `markdown` on the next is a flicker with no
    /// information in it.
    pub fn row_word(&self) -> &'static str {
        match &self.body {
            FileBody::Markdown(_) => "markdown",
            FileBody::Opaque { .. } => "file",
            FileBody::Text(_) => "text",
            FileBody::Error(_) if is_markdown_path(&self.path) => "markdown",
            FileBody::Error(_) => "text",
        }
    }

    /// The typographic mark the pane header and the rail row put before the
    /// name. `\u{b6}` is the design's for a rendered document; a printed file is
    /// not one, and `\u{2261}` says lines where `\u{b6}` says prose.
    pub fn glyph(&self) -> &'static str {
        match &self.body {
            FileBody::Markdown(_) => "\u{b6}",
            FileBody::Error(_) if is_markdown_path(&self.path) => "\u{b6}",
            _ => "\u{2261}",
        }
    }
}

impl Focusable for FileView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A theme change makes the stored colours stale, because the runs carry
        // `Hsla` rather than capture names. Re-colouring the text already in
        // hand is the right repair - re-reading the file would throw away the
        // reader's selection and their scroll position for a reason that has
        // nothing to do with the file. One atomic load per frame to notice, and
        // the parse itself is off-thread and happens once per theme change.
        self.recolor_if_theme_changed(cx);
        let palette = MarkdownPalette::from_active();
        // Collected inside the match and adopted after it: the arm borrows
        // `self.body`, and the layouts belong to `self`.
        let mut fresh_layouts: Option<Vec<gpui::TextLayout>> = None;

        let body = match &self.body {
            FileBody::Error(msg) => div()
                .p(tok::space::XXL)
                .text_color(palette.body)
                .child(msg.clone())
                .into_any_element(),
            FileBody::Text(text) => {
                let mut layouts = Vec::with_capacity(text.lines.len());
                let element = render_text_body(
                    text,
                    palette,
                    self.text_selection,
                    self.text_dragging,
                    &mut layouts,
                    cx,
                );
                fresh_layouts = Some(layouts);
                element
            }
            FileBody::Opaque { bytes, kind } => {
                render_opaque_card(&self.path, *bytes, kind.clone(), cx)
            }
            FileBody::Markdown(ast) => {
                // `w_full()` is load-bearing: list items and paragraphs use
                // `w_full()` on their inner `StyledText` wrappers to opt into
                // soft-wrap. Without a bounded outer width those `w_full()` calls
                // resolve to the intrinsic content width and stop wrapping.
                let mut col = div()
                    .flex()
                    .flex_col()
                    .gap(tok::space::XL)
                    .py(tok::space::BLOCK)
                    .px(tok::space::PAGE)
                    .w_full()
                    .max_w(px(READING_COLUMN_WIDTH));
                for (idx, node) in ast.iter().enumerate() {
                    col = col.child(render_node(RENDER_PATH_ROOT, idx, node, palette));
                }
                // Centred: the column is a measure, and a measure pinned to the
                // left edge of a wide pane reads as a rendering that ran out of
                // content rather than as one that chose its width.
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .child(col)
                    .into_any_element()
            }
        };
        if let Some(layouts) = fresh_layouts {
            self.line_layouts = layouts;
        }

        // Key contexts: `Markdown` always-on; `MarkdownSearch`
        // layered when the find bar is open so Enter/Esc/typing route to
        // the search handlers instead of the document.
        let mut key_ctx = KeyContext::default();
        key_ctx.add("Markdown");
        if self.search_active {
            key_ctx.add("MarkdownSearch");
        }

        // The card for a file there is no viewer for does not scroll: it is
        // one centred block, and a scroll container sizes its child by content,
        // so `size_full` inside one resolves against the block's own height and
        // drops the card to the bottom of the pane.
        let scrolls = !matches!(self.body, FileBody::Opaque { .. });
        let mut scroll_root = div()
            .id(self.element_id.clone())
            .size_full()
            .bg(palette.background)
            .text_color(palette.body)
            .text_size(tok::text::BODY)
            .when(scrolls, |d| {
                d.overflow_y_scroll().track_scroll(&self.scroll_handle)
            })
            // The drag ends wherever the button comes up, including outside
            // the pane - a selection dragged off the bottom edge and released
            // there must not leave every row still listening for the pointer.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_text_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_text_drag(cx)),
            )
            .child(body);
        // The two-axis recipe, from the horizontal-scroll saga written up in
        // `CLAUDE.md`: a printed file's lines scroll sideways inside this
        // column, and without the flag a vertical wheel bleeds into that child
        // and the native Y handler back-fills `delta_y` from `delta.x` under
        // Shift+wheel. It is a raw `StyleRefinement` mutation because GPUI
        // exposes no builder for it.
        scroll_root.style().restrict_scroll_to_axis = Some(true);

        // Visible scrollbar overlay. The markdown viewer fills
        // its pane so we don't have a meaningful first-frame content
        // estimate; pass `None` and let the scrollbar appear after the
        // first paint populates real bounds.
        let bar = crate::widgets::scrollbar::render(
            &self.scroll_handle,
            crate::theme::ui_colors(),
            None,
            "markdown-scrollbar-track",
            "markdown-scrollbar-thumb",
            cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                if let Some(off) = crate::widgets::scrollbar::track_click_offset(
                    &this.scroll_handle,
                    ev.position.y,
                ) {
                    this.scroll_handle.set_offset(Point::new(px(0.), px(off)));
                    cx.notify();
                }
                // The bar is an overlay **over** the lines, and a press that is
                // not stopped here reaches the row underneath as well - which,
                // since a printed file's rows became selectable, meant a click
                // on the track picked a line and the next `⌘C` copied it
                // instead of the file. The thumb below already does this; the
                // track had nothing under it worth stopping for until now.
                cx.stop_propagation();
            }),
            cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                this.scroll_drag = Some(crate::widgets::scrollbar::begin_drag(
                    &this.scroll_handle,
                    ev.position.y,
                ));
                cx.stop_propagation();
            }),
        )
        .filter(|_| scrolls);

        let mut root = div()
            .key_context(key_ctx)
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .on_action(cx.listener(Self::handle_scroll_page_up))
            .on_action(cx.listener(Self::handle_scroll_page_down))
            .on_action(cx.listener(Self::handle_find_open))
            .on_action(cx.listener(Self::handle_find_next))
            .on_action(cx.listener(Self::handle_find_prev))
            .on_action(cx.listener(Self::handle_find_dismiss))
            .on_action(cx.listener(Self::handle_copy))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if let Some(drag) = this.scroll_drag
                    && let Some(off) = crate::widgets::scrollbar::drag_offset(
                        &this.scroll_handle,
                        &drag,
                        ev.position.y,
                    )
                {
                    this.scroll_handle.set_offset(Point::new(px(0.), px(off)));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.scroll_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            );
        if self.search_active {
            root = root.on_key_down(cx.listener(Self::handle_search_key));
        }
        root = root.child(scroll_root);
        if let Some(bar) = bar {
            root = root.child(bar);
        }

        if self.search_active {
            root = root.child(self.render_search_overlay(palette, cx));
        }
        root
    }
}

impl FileView {
    /// The find bar, the same strip the terminal draws: a 30px band across the
    /// top of the body with the accent `⌕`, the query in mono, "3 of 12", `↑`
    /// `↓` and `Aa`.
    ///
    /// It carries no `.*`: markdown search is a substring scan over a harvested
    /// text corpus, and a regex over that corpus would match across a heading
    /// and the paragraph under it - a hit the reader could not see and could
    /// not be scrolled to. `Aa` it does carry, because a case fold is a
    /// property of the same substring scan.
    fn render_search_overlay(
        &self,
        palette: MarkdownPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let total = self.search_matches.len();
        let status: SharedString = if self.search_query.is_empty() {
            SharedString::default()
        } else if total == 0 {
            "No results".into()
        } else {
            format!("{} of {}", self.search_current + 1, total).into()
        };
        let query: SharedString = if self.search_query.is_empty() {
            "Type to search…".into()
        } else {
            SharedString::from(self.search_query.clone())
        };
        let query_color = if self.search_query.is_empty() {
            ui.faint
        } else {
            palette.heading
        };

        let icon_btn =
            |id: &'static str, icon: &'static str| {
                div()
                    .id(id)
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded(tok::radius::TAG)
                    .child(gpui::svg().size(px(14.)).flex_none().path(icon).text_color(
                        if total == 0 {
                            ui.muted.opacity(0.35)
                        } else {
                            ui.muted
                        },
                    ))
            };

        div()
            .id("markdown-find-bar")
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(tok::row::SEARCH)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .px(tok::space::LG)
            .bg(ui.overlay)
            .border_b_1()
            .border_color(ui.border)
            .child(
                gpui::svg()
                    .size(px(12.))
                    .flex_none()
                    .path("icons/tool_search.svg")
                    .text_color(ui.accent),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.))
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::ROW)
                    .text_color(query_color)
                    .child(query),
            )
            .when(!status.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::LABEL)
                        .text_color(ui.muted)
                        .child(status),
                )
            })
            .child(
                icon_btn("markdown-find-prev", "icons/chevron_up.svg").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.handle_find_prev(&crate::MarkdownFindPrev, window, cx);
                    }),
                ),
            )
            .child(
                icon_btn("markdown-find-next", "icons/chevron_down.svg").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.handle_find_next(&crate::MarkdownFindNext, window, cx);
                    }),
                ),
            )
            .child(
                div()
                    .id("markdown-find-case")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .h(px(20.))
                    .px(tok::space::SM)
                    .rounded(tok::radius::TAG)
                    .border_1()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .font_weight(FontWeight::MEDIUM)
                    .bg(if self.search_case_sensitive {
                        ui.accent_surface
                    } else {
                        ui.accent_surface.opacity(0.0)
                    })
                    .border_color(if self.search_case_sensitive {
                        ui.accent_border
                    } else {
                        ui.border_strong
                    })
                    .text_color(if self.search_case_sensitive {
                        ui.accent
                    } else {
                        ui.muted
                    })
                    .child("Aa")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            this.search_case_sensitive = !this.search_case_sensitive;
                            this.recompute_matches();
                            this.scroll_to_current_match();
                            cx.notify();
                        }),
                    ),
            )
    }
}

/// Bound the payload size of a clipboard write. Truncates at
/// `COPY_MAX_BYTES` and appends an ellipsis marker when the cap fires so
/// the user knows the content was clipped. Truncation respects UTF-8
/// codepoint boundaries by walking back to the most recent boundary at or
/// before `COPY_MAX_BYTES`.
fn truncate_for_clipboard(text: &str) -> String {
    if text.len() <= COPY_MAX_BYTES {
        return text.to_string();
    }
    let mut end = COPY_MAX_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push_str("\n…[truncated]");
    out
}

/// Flatten an AST into plain text for substring search. Each block
/// is followed by `\n` so the per-line context heuristic in `handle_copy`
/// can recover the surrounding line. Inline spans concat without separators.
fn harvest_text(nodes: &[MdNode]) -> String {
    let mut buf = String::new();
    walk_text(nodes, &mut buf);
    buf
}

fn lowercase_with_byte_map(input: &str) -> (String, Vec<usize>) {
    let mut lower = String::with_capacity(input.len());
    let mut map = Vec::with_capacity(input.len());
    for (source_idx, ch) in input.char_indices() {
        for lower_ch in ch.to_lowercase() {
            let mut encoded = [0_u8; 4];
            let encoded = lower_ch.encode_utf8(&mut encoded);
            lower.push_str(encoded);
            map.extend(std::iter::repeat_n(source_idx, encoded.len()));
        }
    }
    (lower, map)
}

fn walk_text(nodes: &[MdNode], buf: &mut String) {
    for node in nodes {
        match node {
            MdNode::Heading { spans, .. } | MdNode::Paragraph { spans } => {
                for span in spans {
                    buf.push_str(&span.text);
                }
                buf.push('\n');
            }
            MdNode::CodeBlock { text, .. } => {
                buf.push_str(text);
                if !text.ends_with('\n') {
                    buf.push('\n');
                }
            }
            MdNode::BlockQuote { children } => walk_text(children, buf),
            MdNode::List { items, .. } => {
                for item in items {
                    walk_text(item, buf);
                }
            }
            MdNode::Table { header, rows, .. } => {
                for cell in header {
                    for span in cell {
                        buf.push_str(&span.text);
                    }
                    buf.push('\t');
                }
                buf.push('\n');
                for row in rows {
                    for cell in row {
                        for span in cell {
                            buf.push_str(&span.text);
                        }
                        buf.push('\t');
                    }
                    buf.push('\n');
                }
            }
            MdNode::Rule => buf.push_str("---\n"),
            MdNode::Footnote { label, children } => {
                buf.push_str("[^");
                buf.push_str(label);
                buf.push_str("]: ");
                walk_text(children, buf);
            }
        }
    }
}

/// Compute a stable GPUI element id for one view instance. Multiple tabs may
/// point at the same file, so the path alone is not unique enough for GPUI.
fn make_element_id(path: &std::path::Path) -> SharedString {
    let id = MARKDOWN_VIEW_ID.fetch_add(1, Ordering::Relaxed);
    SharedString::from(format!("markdown-{id}-{}", path.display()))
}

fn render_path_child(parent: u64, idx: usize) -> u64 {
    parent
        .wrapping_mul(1_099_511_628_211)
        .wrapping_add(idx as u64 + 1)
}

/// True when `result` carries a notify event that should trigger a
/// reload of the file we are watching. Event-level errors (`Err`) are ignored;
/// events whose `paths` do not include `target_filename` are siblings in the
/// watched parent directory and ignored.
fn event_is_relevant(
    result: &notify::Result<notify::Event>,
    target_filename: &std::ffi::OsStr,
) -> bool {
    let Ok(event) = result else {
        return false;
    };
    event
        .paths
        .iter()
        .any(|p| p.file_name() == Some(target_filename))
}

/// Outcome of resolving + reading the watched path exactly once. Distinguishes
/// the three states `load_from_disk` needs to surface distinct messages for,
/// without leaking platform-specific `io::ErrorKind`/errno details to callers.
enum ReadOutcome {
    /// File read successfully within the markdown byte cap.
    Bytes(Vec<u8>),
    /// File exceeded the byte cap. Carries the head that was read, so a file
    /// too large to hold whole can still be told from a binary and still show
    /// its first lines - refusing outright sent the one file kind the card
    /// exists for (a big binary) to an error pane with no way out of it.
    TooLarge { head: Vec<u8>, bytes: usize },
    /// The final path component was (or became) a symlink - refused.
    Symlink,
    /// The file does not exist (deleted between watch fire and read).
    NotFound,
    /// Any other IO failure; carries the error for the user-visible message.
    Other(std::io::Error),
}

/// Resolve `path` and read its bytes with a SINGLE name resolution, closing the
/// TOCTOU window (CWE-367) between the old `symlink_metadata` check and the
/// subsequent symlink-following `fs::read`.
///
/// Unix: open with `O_NOFOLLOW` so the kernel refuses (`ELOOP`) if the final
/// component is a symlink - the check and the read are the same syscall, so an
/// attacker cannot swap a regular file for a symlink in between. We read from
/// the resulting fd, never re-resolving the name.
///
/// Windows: open with `FILE_FLAG_OPEN_REPARSE_POINT`, inspect the handle's
/// attributes, and refuse reparse points before reading from that same handle.
fn read_no_follow(path: &std::path::Path) -> ReadOutcome {
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(f) => f,
            // `O_NOFOLLOW` on a symlinked final component fails with ELOOP.
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return ReadOutcome::Symlink,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::NotFound,
            Err(e) => return ReadOutcome::Other(e),
        };
        let mut bytes = Vec::with_capacity(MAX_INPUT_BYTES.min(64 * 1024));
        let mut limited = file.take((MAX_INPUT_BYTES + 1) as u64);
        match limited.read_to_end(&mut bytes) {
            Ok(_) if bytes.len() > MAX_INPUT_BYTES => ReadOutcome::TooLarge {
                bytes: bytes.len(),
                head: bytes,
            },
            Ok(_) => ReadOutcome::Bytes(bytes),
            Err(e) => ReadOutcome::Other(e),
        }
    }
    #[cfg(windows)]
    {
        use std::io::Read;
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        };

        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::NotFound,
            Err(e) => return ReadOutcome::Other(e),
        };
        match file.metadata() {
            Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                return ReadOutcome::Symlink;
            }
            Ok(_) => {}
            Err(e) => return ReadOutcome::Other(e),
        }
        let mut bytes = Vec::with_capacity(MAX_INPUT_BYTES.min(64 * 1024));
        let mut limited = file.take((MAX_INPUT_BYTES + 1) as u64);
        match limited.read_to_end(&mut bytes) {
            Ok(_) if bytes.len() > MAX_INPUT_BYTES => ReadOutcome::TooLarge {
                bytes: bytes.len(),
                head: bytes,
            },
            Ok(_) => ReadOutcome::Bytes(bytes),
            Err(e) => ReadOutcome::Other(e),
        }
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => return ReadOutcome::Symlink,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::NotFound,
            Err(e) => return ReadOutcome::Other(e),
        }
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::NotFound,
            Err(e) => return ReadOutcome::Other(e),
        };
        let mut bytes = Vec::with_capacity(MAX_INPUT_BYTES.min(64 * 1024));
        let mut limited = std::io::Read::take(&mut file, (MAX_INPUT_BYTES + 1) as u64);
        match std::io::Read::read_to_end(&mut limited, &mut bytes) {
            Ok(_) if bytes.len() > MAX_INPUT_BYTES => ReadOutcome::TooLarge {
                bytes: bytes.len(),
                head: bytes,
            },
            Ok(_) => ReadOutcome::Bytes(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ReadOutcome::NotFound,
            Err(e) => ReadOutcome::Other(e),
        }
    }
}

/// Load and parse a markdown file from disk, returning `(ast, error)` where
/// exactly one is `Some`. Free function so unit tests can exercise the
/// initial-load and reload paths without a GPUI context.
///
/// Security: refuses to follow a path that became a symlink since the original
/// open. The path is canonicalised at click time, so the path stored on
/// `FileView` is the real on-disk target (not a symlink). If between
/// initial open and reload an attacker creates a symlink and atomically
/// renames it over the original (e.g. `README.md.evil → /etc/passwd` then
/// `mv README.md.evil README.md`), the post-rename file IS a symlink. We refuse
/// to follow it. The resolution is atomic via `read_no_follow` (`O_NOFOLLOW` on
/// unix) so there is no TOCTOU window between the symlink check and the read
/// (CWE-367). This blocks the information-disclosure attack from adversarial
/// agents writing into the user's project directory. Hard-link attacks remain
/// out of scope (require write access to the disclosure target itself).
/// What this surface found in the file.
///
/// Three things a file can be, and one for the file it could not read. It is
/// one value rather than a pair of `Option`s because a file is one of these:
/// the pair could hold "a document *and* an error", and reading it correctly
/// meant checking in the right order.
enum FileBody {
    /// Markdown, parsed and rendered.
    Markdown(Vec<MdNode>),
    /// Text that is not markdown - shown as its own lines, in mono, with a
    /// number gutter. No syntax highlighting: this surface is for *looking* -
    /// checking what a `package.json` says before typing a command - and the
    /// editor is one keystroke away and highlights better than we would.
    Text(TextBody),
    /// Not text at all. There is no viewer here and inventing one would cost a
    /// dependency to do worse than the OS preview already does; the card names
    /// the file and hands it to the OS.
    Opaque { bytes: u64, kind: SharedString },
    /// Could not be read, with the reason.
    Error(SharedString),
}

/// A text file's lines, and whether they are all of them.
struct TextBody {
    lines: Vec<SharedString>,
    /// `Some(n)` when only the first `n` lines are here.
    cut_after: Option<usize>,
    /// Per-line syntax colours, byte ranges **relative to the line**, ascending
    /// and non-overlapping - which is exactly the shape `highlight_lines`
    /// already produces for the diff, and exactly what `StyledText` wants.
    ///
    /// Same length as `lines`, so a line's runs are `runs[i]`. Empty for a file
    /// with no grammar, one that failed to parse, or one over
    /// `MAX_HIGHLIGHT_BYTES` - all of which render monochrome, which is what
    /// this surface did for every file until now.
    /// Kept in `highlight_lines`' own return shape rather than repacked: this
    /// surface and the diff must be able to be compared byte for byte.
    runs: Vec<Vec<(Range<usize>, Hsla)>>,
    /// The file's extension, lowercased, for a re-colour after a theme change.
    ext: String,
}

/// The word containing byte `at`, or `None` when `at` is not in one.
///
/// A **programmer's** word - alphanumeric plus `_` - and deliberately not
/// `unicode_word_indices`, which was the first thing tried and is wrong here:
/// Unicode's segmentation keeps a `.` between letters inside a word, for
/// abbreviations like "e.g.", so it takes `foo.bar` whole. On code that is the
/// opposite of what a double click is for, and every editor agrees: the
/// gesture exists to pick out one identifier.
///
/// `is_alphanumeric` rather than an ASCII range, so an identifier in a language
/// that does not spell itself in ASCII is still one word.
///
/// **A caret at a word's edge belongs to that word**, which is what `at <= end`
/// says and what makes a click at the very end of an identifier select it. The
/// consequence is worth stating rather than being surprised by: clicking the
/// left half of a single space picks the word before it, and the right half
/// picks the word after, because those are the two caret positions that space
/// offers. A caret strictly inside whitespace - anywhere in a run of two or
/// more spaces - belongs to no word and selects nothing.
fn word_bounds(line: &str, at: usize) -> Option<(usize, usize)> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = at.min(line.len());
    let mut run: Option<usize> = None;
    for (idx, ch) in line.char_indices() {
        match (run, is_word(ch)) {
            (None, true) => run = Some(idx),
            (Some(start), false) => {
                // `at == idx` counts: a click just past the last character of a
                // word is a click on that word, which is what a caret sitting
                // between two characters means.
                if at >= start && at <= idx {
                    return Some((start, idx));
                }
                run = None;
            }
            _ => {}
        }
    }
    // A word that runs to the end of the line closes there.
    run.filter(|start| at >= *start)
        .map(|start| (start, line.len()))
}

/// The two ends of a selection in reading order.
fn ordered(a: TextPoint, b: TextPoint) -> (TextPoint, TextPoint) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Pull `start..end` inside `line` and onto char boundaries.
///
/// Every offset here comes from a hit test against this line's own glyphs, so
/// it is already valid - but a reload can swap the text under a selection
/// between the frame that made it and the frame that reads it, and slicing a
/// `str` off a boundary panics. Two `saturating` walks are cheaper than the
/// alternative, which is a class of crash nobody can reproduce.
fn clamp_to_line(line: &str, start: usize, end: usize) -> (usize, usize) {
    let floor = |mut i: usize| {
        i = i.min(line.len());
        while i > 0 && !line.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (start, end) = (floor(start), floor(end));
    (start.min(end), end)
}

/// A place in a printed file: which line, and how many bytes into it.
///
/// Bytes and not characters, because that is what `TextLayout` answers with and
/// what `str` slices with; every value here is a char boundary by construction,
/// since it comes from a hit test against the shaped glyphs of that very line.
type TextPoint = (usize, usize);

/// The most lines this surface will hold.
///
/// A cut rather than a refusal: a lock file or a bundled asset is exactly the
/// thing someone wants to glance at, and "too large to show" would send them
/// to the shell for a `head` they could have had here. The badge says the file
/// is cut, so nothing is claimed about what is below.
const MAX_TEXT_LINES: usize = 2_000;

/// How much of a file has to be checked before calling it text.
///
/// A NUL byte in the first 8 KiB is what separates a text file from a binary
/// in practice, and it is the same rule `git` uses to decide whether to print
/// a diff. Everything that decodes as UTF-8 without one is text.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Whether "open this" means an editor or the OS, asked of a **path** rather
/// than of a surface that has already read it.
///
/// [`FileView::opens_in_an_editor`] answers the same question from a loaded
/// [`FileBody`]; the Files tree has no body to ask, so it asks disk. One
/// bounded head read per right-click, applying the same rule the loader
/// applies - no NUL in the first 8 KiB and what is there decodes as UTF-8 -
/// so the tree's menu and the surface's own menu cannot come to disagree
/// about what a file is.
///
/// A file that cannot be read at all reads as "not an editor's": the OS
/// handler is the honest fallback when this build knows nothing about it.
pub(crate) fn path_opens_in_an_editor(path: &std::path::Path) -> bool {
    use std::io::Read;
    // Ask what it is before opening it. A FIFO opened read-only **blocks until
    // a writer appears**, and a right-click is not a place the UI may hang; a
    // socket, a device node and a directory are all things there is no editor
    // answer for anyway. `metadata` follows the link, which is right here: the
    // question is about what the path names, and `open_in_default_app` is the
    // one that has to refuse a symlink.
    if !std::fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; BINARY_SNIFF_BYTES];
    let mut filled = 0usize;
    loop {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if filled == head.len() {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    head.truncate(filled);
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&head) {
        Ok(_) => true,
        // A head cut at 8 KiB can split a multi-byte character, which is not a
        // reason to call the file binary. `error_len() == None` is exactly
        // "the input ended mid-character"; any other error is a real one.
        Err(err) => err.error_len().is_none() && filled == BINARY_SNIFF_BYTES,
    }
}

impl FileBody {
    /// The flat text the find bar searches, built on demand.
    fn corpus(&self) -> String {
        match self {
            FileBody::Markdown(nodes) => harvest_text(nodes),
            FileBody::Text(text) => {
                let mut buf = String::new();
                for line in &text.lines {
                    buf.push_str(line);
                    buf.push('\n');
                }
                buf
            }
            FileBody::Opaque { .. } | FileBody::Error(_) => String::new(),
        }
    }
}

/// `JSON`, `TS`, `TOML`, `LOCK` - the extension, uppercased, which is the
/// shortest true thing to put on a mono text pane. A rendered document
/// announces itself by looking rendered; this one does not.
fn extension_badge(path: &std::path::Path) -> Option<SharedString> {
    let ext = path.extension()?.to_str()?;
    // All-digit is a version suffix, not a kind: `libfoo.so.1.2.3` would badge
    // as `3`, which says nothing about the file.
    if ext.is_empty()
        || ext.len() > 12
        || !ext.chars().all(|c| c.is_ascii_alphanumeric())
        || ext.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(SharedString::from(ext.to_ascii_uppercase()))
}

/// Case-insensitive `.md` / `.markdown` / `.mdx`, the extensions this surface
/// renders rather than prints.
fn is_markdown_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "md" || e == "markdown" || e == "mdx"
        })
        .unwrap_or(false)
}

/// The plain-text body: the file's own lines, in mono, behind a number
/// gutter.
///
/// The numbers are the reason this beats `cat` in the shell - the number is
/// what a person says out loud to the agent ("look at line 240") - and they
/// are right-aligned in one gutter with no separator rule, which is the
/// design's own spec.
///
/// No wrapping: a wrapped line makes the number lie about what is beside it.
/// A long line runs off the right and the pane scrolls to it.
fn render_text_body(
    text: &TextBody,
    palette: MarkdownPalette,
    selection: Option<(TextPoint, TextPoint)>,
    dragging: bool,
    layouts: &mut Vec<gpui::TextLayout>,
    cx: &mut Context<FileView>,
) -> AnyElement {
    // The gutter takes the theme's faintest text step, which is the design's
    // own value for it in both themes.
    let gutter_ink = crate::theme::ui_colors().faint;
    // The gutter is sized for the widest number it will show, so it does not
    // step wider halfway down the file.
    let digits = text.lines.len().max(1).to_string().len();
    let gutter = px(digits as f32 * TEXT_DIGIT_WIDTH + TEXT_GUTTER_PAD);
    // The lines scroll sideways rather than wrap: a wrapped line makes the
    // number beside it lie about what it names. The gutter travels with them,
    // which is what `less -N` does and what keeps a number attached to its own
    // line.
    let mut col = div()
        .id("file-view-lines")
        .flex()
        .flex_col()
        .py(tok::space::MD)
        .px(tok::space::MD)
        .overflow_x_scroll();
    // The selection's own ink, at the alpha the text field uses for its own -
    // one answer in the app for "these characters are picked", rather than a
    // second one invented here.
    let accent = crate::theme::ui_colors().accent;
    let picked_bg = gpui::hsla(accent.h, accent.s, accent.l, 0.28);
    let picked = selection.map(|(a, b)| ordered(a, b));
    layouts.clear();
    layouts.reserve(text.lines.len());
    for (idx, line) in text.lines.iter().enumerate() {
        // The picked bytes of *this* line, which is the whole of what the
        // selection means to a row. Painted by the text layout itself as a
        // background run rather than as a quad positioned from
        // `position_for_index`: the glyphs and their highlight then come out of
        // one shaping pass, so the band cannot be a pixel or a frame out of
        // step with the characters it is behind.
        let span = picked.and_then(|(lo, hi)| picked_span(idx, line, lo, hi));
        let styled = highlighted_line(
            line,
            text.runs.get(idx).map(Vec::as_slice).unwrap_or_default(),
            span,
            picked_bg,
        );
        layouts.push(styled.layout().clone());
        col = col.child(
            div()
                .id(("file-view-line", idx))
                .flex()
                .flex_row()
                .items_start()
                .gap(tok::space::MD)
                // Text, not a pointer: this row is something to pick out of,
                // not a control that answers a click.
                .cursor_text()
                // Shift extends from the anchor the way it does in every list
                // in this app and in every editor; a plain press starts a new
                // one. Both are mouse-**down** rather than click, so a drag
                // begins on the same event that would have been a single click
                // had the pointer not moved.
                //
                // Two and three presses are the conventions every editor and
                // every browser share, and a person copying an identifier out
                // of a `package.json` reaches for the first one before they
                // reach for a drag.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        let Some(point) = this.hit(idx, event.position) else {
                            return;
                        };
                        this.text_dragging = true;
                        this.text_selection = match event.click_count {
                            0 | 1 => match this.text_selection {
                                Some((anchor, _)) if event.modifiers.shift => Some((anchor, point)),
                                _ => Some((point, point)),
                            },
                            2 => this.word_at(point),
                            _ => this.line_at(point.0),
                        };
                        cx.notify();
                    }),
                )
                .when(dragging, |row| {
                    row.on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        let Some(point) = this.hit(idx, event.position) else {
                            return;
                        };
                        if let Some((anchor, head)) = this.text_selection
                            && head != point
                        {
                            this.text_selection = Some((anchor, point));
                            cx.notify();
                        }
                    }))
                })
                .child(
                    div()
                        .flex_none()
                        .w(gutter)
                        .text_align(gpui::TextAlign::Right)
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::ROW)
                        .text_color(gutter_ink)
                        .child(SharedString::from((idx + 1).to_string())),
                )
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::ROW)
                        .text_color(palette.body)
                        .child(styled),
                ),
        );
    }
    if let Some(cut) = text.cut_after {
        col = col.child(
            div()
                .pt(tok::space::LG)
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                .text_color(gutter_ink)
                .child(SharedString::from(format!(
                    "first {cut} lines \u{b7} open the file to see the rest"
                ))),
        );
    }
    col.into_any_element()
}

/// Which bytes of line `idx` fall inside the selection `lo..hi`.
///
/// `None` for a line outside it and for a caret with no width - a click that
/// picked nothing must not paint a band.
fn picked_span(idx: usize, line: &str, lo: TextPoint, hi: TextPoint) -> Option<Range<usize>> {
    if idx < lo.0 || idx > hi.0 {
        return None;
    }
    let start = if idx == lo.0 { lo.1 } else { 0 };
    let end = if idx == hi.0 { hi.1 } else { line.len() };
    let (start, end) = clamp_to_line(line, start, end);
    (end > start).then_some(start..end)
}

/// One line of the file, coloured by its syntax runs and by the selection.
///
/// A `StyledText` and **not** a row of coloured `div`s, which was the obvious
/// shape and is the wrong one: a line's indentation is not covered by any
/// capture, so it would land in a plain span of its own, and splitting a line
/// into flex children puts the layout in charge of the space between them. One
/// text element keeps the line one shaped run of glyphs - the indentation, the
/// alignment, and the horizontal scroll all stay exactly what they were.
///
/// It is also what makes the selection work: the element keeps a `TextLayout`,
/// and that is what answers `index_for_position` for the hit test and paints
/// `picked` as a background run in the same pass as the glyphs.
///
/// `with_highlights` inherits the ambient style for everything a run does not
/// cover, which is why the colour of ordinary code is still set on the parent
/// `div` and is not repeated here.
fn highlighted_line(
    line: &SharedString,
    runs: &[(Range<usize>, Hsla)],
    picked: Option<Range<usize>>,
    picked_bg: Hsla,
) -> gpui::StyledText {
    let text = gpui::StyledText::new(line.clone());
    let highlights = merge_line_styles(line, runs, picked, picked_bg);
    if highlights.is_empty() {
        return text;
    }
    text.with_highlights(highlights)
}

/// Fold the syntax runs and the selection band into one ascending,
/// non-overlapping list of styles.
///
/// `with_highlights` walks its input in order and slices the text at each
/// range, so the two sources cannot simply be concatenated: a selection over
/// half a keyword overlaps that keyword's colour run. Cutting at every boundary
/// of either set and asking both about each piece is the whole of it - the
/// colour survives inside the band, and the band survives across a colour
/// change, which is what "highlighted code, with a selection over it" has to
/// mean.
///
/// Pure and total: it never returns a range outside `line`, never an empty one,
/// and never one that is not a char boundary - which matters because
/// `with_highlights` debug-asserts all three and would panic on a debug build
/// rather than draw something slightly wrong.
fn merge_line_styles(
    line: &str,
    runs: &[(Range<usize>, Hsla)],
    picked: Option<Range<usize>>,
    picked_bg: Hsla,
) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
    let usable = |range: &Range<usize>| {
        range.start < range.end
            && range.end <= line.len()
            && line.is_char_boundary(range.start)
            && line.is_char_boundary(range.end)
    };
    let runs: Vec<&(Range<usize>, Hsla)> = runs.iter().filter(|(r, _)| usable(r)).collect();
    let picked = picked.filter(usable);
    if runs.is_empty() && picked.is_none() {
        return Vec::new();
    }

    // Every edge either set introduces. Sorted and deduplicated, consecutive
    // pairs are the pieces over which both answers are constant.
    let mut cuts: Vec<usize> = Vec::with_capacity(runs.len() * 2 + 2);
    for (range, _) in &runs {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    if let Some(range) = &picked {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::with_capacity(cuts.len());
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        // The runs are ascending and non-overlapping (`resolve_runs` guarantees
        // it), so at most one can cover a piece; `find` is not a shortcut past
        // an ambiguity, there is none to be past.
        let color = runs
            .iter()
            .find(|(range, _)| range.start <= start && range.end >= end)
            .map(|(_, color)| *color);
        let background = picked
            .as_ref()
            .filter(|range| range.start <= start && range.end >= end)
            .map(|_| picked_bg);
        if color.is_none() && background.is_none() {
            // A gap between runs, outside the selection: the ambient style is
            // already right for it and a style that changes nothing is a slice
            // of the text for no reason.
            continue;
        }
        out.push((
            start..end,
            gpui::HighlightStyle {
                color,
                background_color: background,
                ..Default::default()
            },
        ));
    }
    out
}

/// A file that is not text: the card, and the two things the OS does better.
///
/// No viewer, and that is the decision rather than a gap: an image
/// viewer costs a dependency to do worse than the OS preview, and a
/// half-hearted one earns nothing. The card states what is known - the name,
/// the size, what kind of thing it is - and hands the file over.
fn render_opaque_card(
    path: &std::path::Path,
    bytes: u64,
    kind: SharedString,
    cx: &mut Context<FileView>,
) -> AnyElement {
    let ui = crate::theme::ui_colors();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let reveal_path = path.parent().map(std::path::Path::to_path_buf);
    let open_path = path.to_path_buf();
    let button = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .flex()
            .items_center()
            .h(tok::row::SEARCH)
            .px(tok::space::BLOCK)
            .rounded(tok::radius::CONTROL)
            .border_1()
            .border_color(ui.border)
            .text_size(tok::text::ROW)
            .text_color(ui.text_body)
            .cursor_pointer()
            .child(label)
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(tok::space::XL)
        .child(
            div()
                .text_size(tok::text::ROW)
                .text_color(ui.text)
                .child(SharedString::from(name)),
        )
        .child(
            div()
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                .text_color(ui.muted)
                .child(SharedString::from(format!(
                    "{kind} \u{b7} {}",
                    human_bytes(bytes)
                ))),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(tok::space::MD)
                .child(
                    button("file-view-reveal", "Reveal in file manager").on_click(cx.listener(
                        move |_, _: &gpui::ClickEvent, _w, _cx| {
                            if let Some(dir) = reveal_path.as_deref()
                                && let Err(err) =
                                    crate::app::workspace_ops::reveal_in_file_manager(dir)
                            {
                                log::warn!("file view: could not reveal {}: {err}", dir.display());
                            }
                        },
                    )),
                )
                .child(
                    button("file-view-open", "Open with the default app").on_click(cx.listener(
                        move |_, _: &gpui::ClickEvent, _w, _cx| {
                            // The symlink guard this used to spell out inline
                            // lives in the helper now, because the surface's
                            // own menu offers the same action and two doors
                            // onto one file must not disagree about whether it
                            // is safe to follow.
                            if let Err(err) =
                                crate::app::workspace_ops::open_in_default_app(&open_path)
                            {
                                log::warn!("file view: {err}");
                            }
                        },
                    )),
                ),
        )
        .into_any_element()
}

/// `12 KB`, `3.4 MB`. Bytes below a kilobyte are printed whole - a 40-byte
/// file rounding to `0 KB` reads as an empty one.
fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes < KB {
        format!("{bytes} bytes")
    } else if bytes < MB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    }
}

/// The mono cell width the gutter is sized from, and the space between the
/// number and the line. Both are the gutter's own geometry rather than a token
/// - a token would be a spacing step, and this is a measurement of type.
const TEXT_DIGIT_WIDTH: f32 = 8.0;
const TEXT_GUTTER_PAD: f32 = 4.0;

/// Read the file, and - when a theme snapshot is handed in - colour it.
///
/// `syntax` is `Option` because two callers have no theme to give: the tests,
/// and any future path that wants the text without paying for a parse. `None`
/// is monochrome, which is what this surface did for every file before syntax
/// colouring.
///
/// **The colours come from the diff's engine** (`diff::highlight_lines`), not
/// from a second one and not from the donor's incremental `CodeHighlighter`.
/// That one keeps a tree-sitter tree alive across keystrokes under a 1ms parse
/// budget, which is the right shape for an editor and pure overhead for a file
/// that is parsed once and never edited - and it consumes this very function
/// underneath. So the whole of "syntax highlighting in the file surface" is one
/// call, seven grammars we already ship, and no new dependency.
fn load_from_disk(path: &std::path::Path, syntax: Option<crate::diff::FileSyntax>) -> FileBody {
    let (bytes, over_cap) = match read_no_follow(path) {
        ReadOutcome::Bytes(bytes) => (bytes, false),
        // Over the cap and still answered: the head decides what kind of thing
        // it is, and the line cut below decides how much of it is shown. Only
        // markdown still refuses, because it has to parse the whole document
        // to render any of it.
        ReadOutcome::TooLarge { head, bytes } => {
            if is_markdown_path(path) {
                return FileBody::Error(
                    format!(
                        "Markdown file too large ({} KB) - max {} KB. Open externally to view.",
                        bytes / 1024,
                        MAX_INPUT_BYTES / 1024
                    )
                    .into(),
                );
            }
            (head, true)
        }
        ReadOutcome::Symlink => {
            return FileBody::Error(
                "File path was replaced by a symlink - refusing to read.".into(),
            );
        }
        // Deletion during the session shows a stable message
        // and keeps the pane open (no crash, no auto-close).
        ReadOutcome::NotFound => return FileBody::Error("File deleted".into()),
        ReadOutcome::Other(e) => {
            return FileBody::Error(format!("Could not read file: {e}").into());
        }
    };
    let size = std::fs::metadata(path).map_or(bytes.len() as u64, |m| m.len());
    // The binary test comes first and is cheap: a NUL in the head settles it
    // without decoding the rest.
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return FileBody::Opaque {
            bytes: size,
            kind: extension_badge(path).unwrap_or_else(|| SharedString::from("binary")),
        };
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // A cut head can split a multi-byte character, which is not a reason
        // to call the file binary: decode what is whole and drop the tail.
        Err(err) if over_cap => {
            let valid = err.utf8_error().valid_up_to();
            let mut bytes = err.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
        Err(_) => {
            return FileBody::Opaque {
                bytes: size,
                kind: extension_badge(path).unwrap_or_else(|| SharedString::from("binary")),
            };
        }
    };
    if is_markdown_path(path) {
        return match parse_with_limit(&text) {
            Ok(nodes) => FileBody::Markdown(nodes),
            Err(ParseError::TooLarge { bytes, limit }) => FileBody::Error(
                format!(
                    "Markdown file too large ({} KB) - max {} KB. Open externally to view.",
                    bytes / 1024,
                    limit / 1024
                )
                .into(),
            ),
        };
    }
    let mut lines: Vec<SharedString> = Vec::new();
    let mut cut_after = None;
    for line in text.lines() {
        if lines.len() == MAX_TEXT_LINES {
            cut_after = Some(MAX_TEXT_LINES);
            break;
        }
        lines.push(SharedString::from(line.to_string()));
    }
    // A file the read itself could not hold whole is cut even if its head fits
    // in the line budget - saying otherwise would claim these are all the lines
    // there are.
    if over_cap && cut_after.is_none() {
        cut_after = Some(lines.len());
    }
    let ext = file_extension(path);
    // The whole text is parsed even when the view is cut, because a parser
    // given half a file is given a different file: the cut could land inside a
    // string or a block, and the lines above it would be coloured by the
    // grammar's recovery rather than by the code. The runs past the cut are
    // dropped, not the context that made the ones above it right.
    let runs = highlight_for(&text, &ext, syntax, lines.len());
    FileBody::Text(TextBody {
        lines,
        cut_after,
        runs,
        ext,
    })
}

/// The active theme's syntax map plus the generation it belongs to.
///
/// Must be called on the render thread - `active_theme()` polls a file mtime.
fn syntax_snapshot() -> (crate::diff::FileSyntax, u64) {
    let theme = crate::theme::active_theme();
    (
        crate::diff::FileSyntax::from_theme(&theme),
        crate::theme::theme_generation(),
    )
}

/// The file's extension, lowercased, as `grammar_for_ext` wants it.
fn file_extension(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Per-line colour runs for `text`, trimmed to the `shown` lines the view holds.
///
/// Always the same length as the line list, so `runs[i]` is line `i` and the
/// render path needs no bounds dance. A file with no grammar, a parse failure
/// and a file over `MAX_HIGHLIGHT_BYTES` all land here as empty runs, which is
/// the monochrome this surface has always drawn.
fn highlight_for(
    text: &str,
    ext: &str,
    syntax: Option<crate::diff::FileSyntax>,
    shown: usize,
) -> Vec<Vec<(Range<usize>, Hsla)>> {
    let Some(syntax) = syntax else {
        return vec![Vec::new(); shown];
    };
    let mut runs = crate::diff::highlight_lines(text, ext, &syntax);
    runs.truncate(shown);
    runs.resize_with(shown, Vec::new);
    runs
}

// ---------------------------------------------------------------------------
// Render helpers - pure functions, no `&mut Context` needed.
// ---------------------------------------------------------------------------

fn render_node(
    parent_path: u64,
    idx: usize,
    node: &MdNode,
    palette: MarkdownPalette,
) -> AnyElement {
    let path = render_path_child(parent_path, idx);
    match node {
        MdNode::Heading { level, spans } => render_heading(*level, spans, palette),
        MdNode::Paragraph { spans } => render_paragraph(spans, palette).into_any_element(),
        MdNode::CodeBlock { lang: _, text } => render_code_block(path, text, palette),
        MdNode::BlockQuote { children } => render_blockquote(path, children, palette),
        MdNode::List {
            ordered_start,
            items,
        } => render_list(path, *ordered_start, items, palette),
        MdNode::Table {
            alignments,
            header,
            rows,
        } => render_table(alignments, header, rows, palette),
        MdNode::Rule => render_rule(palette),
        MdNode::Footnote { label, children } => render_footnote(path, label, children, palette),
    }
}

/// Build a single `StyledText` element from a sequence of inline spans.
///
/// Why one element instead of N child divs (the previous approach): GPUI's
/// soft-wrap only kicks in *inside* a `StyledText` - a paragraph rendered as
/// many sibling divs lays out as a row that grows past the parent width and
/// never breaks. Coalescing every span of a paragraph into a single element
/// with per-run styling lets the text shaper reflow naturally inside the
/// pane's bounded width.
///
/// `base_color` and `base_weight` apply to plain (non-styled) spans; per-span
/// flags (strong, emphasis, code, link, strikethrough) override on a per-run
/// basis. Returns `None` when the spans contain no text - caller can then
/// skip the element entirely instead of painting an empty StyledText.
///
/// Known limitation: link spans are styled (link color + underline) but are
/// not yet clickable. The previous implementation routed clicks via a per-
/// span `on_mouse_down`, which loses inline reflow. Restoring clickability
/// here requires hit-testing the StyledText's laid-out runs (Zed's approach
/// in `crates/markdown/src/markdown.rs` - `RenderedText` + `LinkAndRange`).
/// Tracked as follow-up work; until then `Cmd/Ctrl-click` on a `.md` path
/// from a terminal pane remains the supported way to open another markdown.
///
/// SECURITY: when that rewire lands, the resolved `span.link_url`
/// MUST be passed through `crate::markdown::security::validate_link_url(url)?`
/// before reaching `open::that`. `.md` content is potentially hostile, so a
/// raw `file://`/`javascript:`/`data:` URL must never hit `xdg-open`. The
/// allow-list is regression-tested by `security::tests::allowlist_is_http_https_only`.
fn build_styled_text(
    spans: &[Span],
    palette: MarkdownPalette,
    base_color: Hsla,
    base_weight: FontWeight,
) -> Option<StyledText> {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::with_capacity(spans.len());
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        let len = span.text.len();
        let is_code = span.style.code;
        let family: SharedString = if is_code {
            crate::ui_tokens::font::MONO.into()
        } else {
            ".SystemUIFont".into()
        };
        let weight = if span.style.strong {
            FontWeight::BOLD
        } else {
            base_weight
        };
        let style = if span.style.emphasis {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        let mut color = if is_code { palette.code_fg } else { base_color };
        let bg = if is_code { Some(palette.code_bg) } else { None };
        let mut underline: Option<UnderlineStyle> = None;
        if span.link_url.is_some() {
            color = palette.link;
            underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            });
        }
        let strikethrough = if span.style.strikethrough {
            Some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            })
        } else {
            None
        };
        runs.push(TextRun {
            len,
            font: Font {
                family,
                features: FontFeatures::default(),
                fallbacks: None,
                weight,
                style,
            },
            color,
            background_color: bg,
            underline,
            strikethrough,
        });
        text.push_str(&span.text);
    }
    if text.is_empty() {
        None
    } else {
        Some(StyledText::new(text).with_runs(runs))
    }
}

/// The design's heading scale, taken off the token scale rather than off its
/// own pixel values: it asks for 22 and 15 at weight 600, and 22 is not a step
/// this app has - `text::DISPLAY` (19) is the largest, and it is the step the
/// design itself uses for the one largest string on a screen. Everything below
/// h2 continues down the same scale, which is what keeps six heading levels
/// from flattening into three.
fn render_heading(level: HeadingLevel, spans: &[Span], palette: MarkdownPalette) -> AnyElement {
    let (size, weight, top_gap) = match level {
        HeadingLevel::H1 => (tok::text::DISPLAY, FontWeight::SEMIBOLD, tok::space::MD),
        HeadingLevel::H2 => (tok::text::HEADING, FontWeight::SEMIBOLD, tok::space::DIALOG),
        HeadingLevel::H3 => (tok::text::TITLE, FontWeight::SEMIBOLD, tok::space::XL),
        HeadingLevel::H4 => (tok::text::BODY, FontWeight::SEMIBOLD, tok::space::MD),
        HeadingLevel::H5 | HeadingLevel::H6 => {
            (tok::text::ROW, FontWeight::SEMIBOLD, tok::space::SM)
        }
    };
    let mut row = div().w_full().text_size(size).pt(top_gap);
    if let Some(styled) = build_styled_text(spans, palette, palette.heading, weight) {
        row = row.child(styled);
    }
    row.into_any_element()
}

fn render_paragraph(spans: &[Span], palette: MarkdownPalette) -> impl IntoElement {
    let mut row = div().w_full();
    if let Some(styled) = build_styled_text(spans, palette, palette.body, FontWeight::NORMAL) {
        row = row.child(styled);
    }
    row
}

fn render_code_block(path: u64, text: &str, palette: MarkdownPalette) -> AnyElement {
    // Code blocks contain pre-formatted content that must NOT soft-wrap
    // (preserves indentation + intent). Long lines previously clipped at the
    // pane edge; following Zed's `markdown.rs` pattern (`overflow_x_scroll`
    // + a stable id), each block becomes its own horizontally scrollable
    // container. The id is derived from the AST path, not the text, so large
    // blocks do not get hashed during render.
    div()
        .id(("md-code-block", path))
        .bg(palette.code_bg)
        .text_color(palette.code_fg)
        .border_1()
        .border_color(palette.code_border)
        .font_family(crate::ui_tokens::font::MONO)
        .text_size(tok::mono::INLINE)
        .px(tok::space::XL)
        .py(tok::space::MD)
        .rounded(tok::radius::PANEL)
        .w_full()
        .overflow_x_scroll()
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

fn render_blockquote(path: u64, children: &[MdNode], palette: MarkdownPalette) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .gap(tok::space::MD)
        .border_l_2()
        .border_color(palette.blockquote_border)
        .pl(tok::space::XL)
        .w_full()
        .text_color(palette.blockquote_text);
    for (idx, child) in children.iter().enumerate() {
        col = col.child(render_node(path, idx, child, palette));
    }
    col.into_any_element()
}

fn render_list(
    path: u64,
    ordered_start: Option<u64>,
    items: &[Vec<MdNode>],
    palette: MarkdownPalette,
) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .gap(tok::space::XS)
        .pl(tok::space::SECTION)
        .w_full();
    for (idx, item) in items.iter().enumerate() {
        let item_path = render_path_child(path, idx);
        let marker: SharedString = match ordered_start {
            Some(start) => format!("{}.", start.saturating_add(idx as u64)).into(),
            None => "•".into(),
        };
        // `w_full()` bounds the row to its parent. The marker is fixed-width
        // and `flex_shrink_0` so it never collapses; the body claims the rest
        // (`flex_1`) and `min_w(px(0.))` lets it shrink below its intrinsic
        // text width - without this, a flex item's min-width defaults to
        // `auto` (= content size) and long lines push the row past the
        // viewport instead of wrapping inside StyledText.
        let mut item_row = div().flex().flex_row().gap(tok::space::MD).w_full();
        item_row = item_row.child(
            div()
                .w(px(20.))
                .flex_shrink_0()
                .text_color(palette.list)
                .child(marker),
        );
        let mut item_body = div()
            .flex()
            .flex_col()
            .gap(tok::space::XS)
            .flex_1()
            .min_w(px(0.));
        // The design reads a list one step below prose (`#b9bfc7` against
        // `#cdd2d8`) - the only thing separating a run of items from a run of
        // sentences. A paragraph paints its own colour, so the step has to
        // reach it as the palette it is rendered with, not as an inherited
        // style on the row.
        let item_palette = MarkdownPalette {
            body: palette.list,
            ..palette
        };
        for (cidx, child) in item.iter().enumerate() {
            item_body = item_body.child(render_node(item_path, cidx, child, item_palette));
        }
        item_row = item_row.child(item_body);
        col = col.child(item_row);
    }
    col.into_any_element()
}

/// Column count for a markdown table: the max of header arity and the longest
/// data row, capped to a renderer-safe maximum.
///
/// The input is untrusted file content (MAX_INPUT_BYTES = 10 MB admits
/// a multi-million-column delimiter row). `grid_cols` with thousands of
/// columns is not useful UI, so we render a bounded prefix.
fn table_col_count(header: &[Vec<Span>], rows: &[Vec<Vec<Span>>]) -> u16 {
    let cols = header
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    u16::try_from(cols)
        .unwrap_or(MAX_RENDERED_TABLE_COLUMNS)
        .min(MAX_RENDERED_TABLE_COLUMNS)
}

fn render_table(
    _alignments: &[Alignment],
    header: &[Vec<Span>],
    rows: &[Vec<Vec<Span>>],
    palette: MarkdownPalette,
) -> AnyElement {
    // Column count: max of header arity and the longest data row. Empty
    // tables (zero header + zero rows, or rows with zero cells) bail out
    // before invoking grid_cols(0) which would be a runtime no-op.
    let cols = table_col_count(header, rows);
    if cols == 0 {
        return div().into_any_element();
    }

    // Per Zed's `MarkdownElement` (crates/markdown/src/markdown.rs:1981):
    // CSS grid + equal-fraction columns + `overflow_hidden` is the only
    // layout that prevents wide cells from blowing the table past the pane
    // width. `flex_row` rows would let any single cell push the row wider
    // than its parent - which is exactly what the original implementation
    // did. With grid the row width is always `w_full`, columns are 1fr each,
    // and StyledText cell content reflows inside its column.
    let mut table = div()
        .grid()
        .grid_cols(cols)
        .w_full()
        .overflow_hidden()
        .border_1()
        .border_color(palette.rule)
        .rounded(tok::radius::BADGE);

    if !header.is_empty() {
        for cell in header.iter().take(cols as usize) {
            table = table.child(render_table_cell(cell, palette, true));
        }
    }
    for row in rows {
        for cell in row.iter().take(cols as usize) {
            table = table.child(render_table_cell(cell, palette, false));
        }
    }
    table.into_any_element()
}

fn render_table_cell(
    spans: &[Span],
    palette: MarkdownPalette,
    is_header: bool,
) -> impl IntoElement {
    let weight = if is_header {
        FontWeight::SEMIBOLD
    } else {
        FontWeight::NORMAL
    };
    let mut cell = div()
        .px(tok::space::MD)
        .py(tok::space::XS)
        .border_b_1()
        .border_r_1()
        .border_color(palette.rule)
        .text_color(palette.body);
    if is_header {
        cell = cell.bg(palette.code_bg);
    }
    if let Some(styled) = build_styled_text(spans, palette, palette.body, weight) {
        cell = cell.child(styled);
    }
    cell
}

fn render_rule(palette: MarkdownPalette) -> AnyElement {
    div()
        .h(px(1.))
        .my(tok::space::XS)
        .bg(palette.rule)
        .into_any_element()
}

fn render_footnote(
    path: u64,
    label: &str,
    children: &[MdNode],
    palette: MarkdownPalette,
) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .gap(tok::space::XS)
        .w_full()
        .text_color(palette.blockquote_text)
        .text_size(tok::text::ROW);
    col = col.child(
        div()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(SharedString::from(format!("[^{}]", label))),
    );
    for (idx, child) in children.iter().enumerate() {
        col = col.child(render_node(path, idx, child, palette));
    }
    col.into_any_element()
}

// ---------------------------------------------------------------------------
// Tests - exercise the data-only paths (non-rendering) so the file doesn't
// drift from `parser` without notice. Render paths require a GPUI context
// and are verified manually per repo convention.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// The Files tree's menu and the surface's own must not come to disagree
    /// about what a file is, so this asks disk with the loader's rule.
    #[test]
    fn a_path_is_an_editors_when_its_head_is_text() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let text = tmp.path().join("notes.md");
        fs::write(&text, "# hello\n\nworld\n").expect("write");
        assert!(path_opens_in_an_editor(&text));

        // Git's own rule: a NUL in the head settles it.
        let binary = tmp.path().join("blob.bin");
        fs::write(&binary, [0x89u8, 0x50, 0x00, 0x0d]).expect("write");
        assert!(!path_opens_in_an_editor(&binary));

        // Not valid UTF-8, and short enough that the head is the whole file -
        // so the split-character escape hatch must not let it through.
        let latin1 = tmp.path().join("latin1.txt");
        fs::write(&latin1, [0xffu8, 0xfe, 0x41]).expect("write");
        assert!(!path_opens_in_an_editor(&latin1));

        // An empty file decodes as text, which is what the surface shows too.
        let empty = tmp.path().join("empty.txt");
        fs::write(&empty, b"").expect("write");
        assert!(path_opens_in_an_editor(&empty));
    }

    /// A file long enough to be cut mid-character is still text: the head ends
    /// inside a multi-byte character, which is the one decode error that says
    /// nothing about the file.
    #[test]
    fn a_character_split_by_the_head_is_not_a_binary_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("wide.txt");
        // Pad so the 8 KiB boundary lands in the middle of a 3-byte character.
        let mut body = "a".repeat(BINARY_SNIFF_BYTES - 1);
        body.push_str(&"\u{4e2d}".repeat(64));
        fs::write(&path, body).expect("write");
        assert!(path_opens_in_an_editor(&path));
    }

    /// Nothing to read is not "an editor's": the OS handler is the honest
    /// fallback for a path this build knows nothing about.
    #[test]
    fn an_unreadable_path_is_left_to_the_os() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!path_opens_in_an_editor(&tmp.path().join("nothing-here")));
    }

    /// A pathological table must be capped before it reaches grid_cols.
    /// A genuinely empty table still reports 0.
    #[test]
    fn table_col_count_caps_pathological_tables() {
        let huge: Vec<Vec<Span>> = vec![Vec::new(); u16::MAX as usize + 2];
        assert_eq!(
            table_col_count(&[], std::slice::from_ref(&huge)),
            MAX_RENDERED_TABLE_COLUMNS
        );
        // Empty table reports 0 so the `cols == 0` bail still fires.
        assert_eq!(table_col_count(&[], &[]), 0);
        // Header arity counts too, and a normal small table is unchanged.
        let header: Vec<Vec<Span>> = vec![Vec::new(); 3];
        assert_eq!(table_col_count(&header, &[]), 3);
    }

    #[test]
    fn lowercase_map_preserves_source_offsets_for_unicode_search() {
        let corpus = "Cafe İSTANBUL";
        let (lower, map) = lowercase_with_byte_map(corpus);
        let pos = lower.find("i").expect("lowercase dotted I should match i");
        assert_eq!(&corpus[map[pos]..map[pos] + "İ".len()], "İ");
    }

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write");
    }

    /// The AST of a body that is markdown, for the tests that assert on it.
    fn ast_of(body: &FileBody) -> &[MdNode] {
        match body {
            FileBody::Markdown(nodes) => nodes,
            other => panic!("expected markdown, got {}", body_kind(other)),
        }
    }

    fn body_kind(body: &FileBody) -> &'static str {
        match body {
            FileBody::Markdown(_) => "markdown",
            FileBody::Text(_) => "text",
            FileBody::Opaque { .. } => "opaque",
            FileBody::Error(_) => "error",
        }
    }

    fn error_of(body: &FileBody) -> &str {
        match body {
            FileBody::Error(msg) => msg.as_ref(),
            other => panic!("expected an error, got {}", body_kind(other)),
        }
    }

    #[test]
    fn loads_existing_file_into_ast() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("doc.md");
        write(&path, b"# Hello\n");
        let body = load_from_disk(&path, None);
        assert!(matches!(
            ast_of(&body).first(),
            Some(MdNode::Heading { .. })
        ));
    }

    #[test]
    fn reload_picks_up_modified_content() {
        // A second read after content change must reflect the new AST.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("live.md");
        write(&path, b"# v1\n");
        let body_v1 = load_from_disk(&path, None);
        let v1_text = match ast_of(&body_v1).first() {
            Some(MdNode::Heading { spans, .. }) => {
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            }
            _ => panic!("expected heading"),
        };
        assert_eq!(v1_text, "v1");

        write(&path, b"# v2\n");
        let body_v2 = load_from_disk(&path, None);
        let v2_text = match ast_of(&body_v2).first() {
            Some(MdNode::Heading { spans, .. }) => {
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            }
            _ => panic!("expected heading"),
        };
        assert_eq!(v2_text, "v2", "reload must reflect new content");
    }

    #[test]
    fn deleted_file_surfaces_file_deleted_message() {
        // Deletion during the session must produce the literal
        // "File deleted" message, not a crash or auto-close.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("doomed.md");
        write(&path, b"# alive\n");
        fs::remove_file(&path).expect("rm");
        assert_eq!(error_of(&load_from_disk(&path, None)), "File deleted");
    }

    #[test]
    fn oversized_file_shows_size_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("huge.md");
        let bytes = vec![b'a'; MAX_INPUT_BYTES + 1];
        write(&path, &bytes);
        let body = load_from_disk(&path, None);
        let msg = error_of(&body);
        assert!(msg.contains("too large"), "expected size warning: {msg}");
    }

    #[test]
    fn a_file_that_is_not_utf8_gets_the_card_rather_than_an_error() {
        // It used to say "not valid UTF-8 - cannot render as markdown", which
        // is a complaint about the file rather than an answer about it. There
        // is no viewer here and inventing one would cost a
        // dependency to do worse than the OS preview, so the card names the
        // file and hands it over.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("not_utf8.md");
        // 0xFF is invalid as a leading byte in UTF-8.
        write(&path, &[0xFF, 0xFE, 0xFD]);
        assert!(matches!(
            load_from_disk(&path, None),
            FileBody::Opaque { bytes: 3, .. }
        ));
    }

    #[test]
    fn a_file_that_is_not_markdown_is_printed_with_its_own_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("package.json");
        write(&path, b"{\n  \"name\": \"splitlane\"\n}\n");
        match load_from_disk(&path, None) {
            FileBody::Text(text) => {
                assert_eq!(text.lines.len(), 3);
                assert_eq!(text.lines[1].as_ref(), "  \"name\": \"splitlane\"");
                assert!(text.cut_after.is_none());
            }
            other => panic!("expected text, got {}", body_kind(&other)),
        }
    }

    #[test]
    fn a_long_file_is_cut_rather_than_refused() {
        // A lock file or a bundled asset is exactly the thing someone wants to
        // glance at, and "too large to show" would send them to the shell for
        // a `head` they could have had here.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("yarn.lock");
        let body: String = (0..MAX_TEXT_LINES + 500)
            .map(|n| format!("line {n}\n"))
            .collect();
        write(&path, body.as_bytes());
        match load_from_disk(&path, None) {
            FileBody::Text(text) => {
                assert_eq!(text.lines.len(), MAX_TEXT_LINES);
                assert_eq!(text.cut_after, Some(MAX_TEXT_LINES));
            }
            other => panic!("expected text, got {}", body_kind(&other)),
        }
    }

    #[test]
    fn a_nul_in_the_head_makes_it_a_file_rather_than_text() {
        // The same rule git uses to decide whether to print a diff. It comes
        // before the UTF-8 decode because it settles the question on the head
        // alone.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("thing.bin");
        write(&path, b"MZ\x00\x00some bytes");
        assert!(matches!(
            load_from_disk(&path, None),
            FileBody::Opaque { .. }
        ));
    }

    #[test]
    fn markdown_is_still_rendered_and_only_by_extension() {
        // The body is decided by the file's name, not by whether the text
        // happens to parse: `# heading` is a valid line of a shell script.
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = tmp.path().join("build.sh");
        write(&script, b"# not a heading\necho hi\n");
        assert!(matches!(load_from_disk(&script, None), FileBody::Text(_)));

        let doc = tmp.path().join("README.MD");
        write(&doc, b"# a heading\n");
        assert!(matches!(load_from_disk(&doc, None), FileBody::Markdown(_)));
    }

    /// The colours reach the lines, and they are the diff's own.
    ///
    /// The claim worth testing is not "something is coloured" but that one file
    /// gets one answer in both surfaces: the file view calls
    /// `diff::highlight_lines` directly rather than owning a second grammar
    /// table, so a divergence here would mean the call had been replaced by
    /// something else.
    #[test]
    fn a_source_file_is_coloured_by_the_diffs_own_engine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.rs");
        std::fs::write(&path, "fn main() {\n    let x = 1;\n}\n").expect("write");
        let syntax = crate::diff::FileSyntax::from_theme(&crate::theme::one_dark());

        let FileBody::Text(body) = load_from_disk(&path, Some(syntax)) else {
            panic!("a .rs file is text");
        };
        assert_eq!(body.runs.len(), body.lines.len(), "one run list per line");
        assert!(
            body.runs.iter().any(|line| !line.is_empty()),
            "a Rust file has something to colour"
        );
        assert_eq!(body.ext, "rs");

        let expected =
            crate::diff::highlight_lines("fn main() {\n    let x = 1;\n}\n", "rs", &syntax);
        assert_eq!(
            body.runs,
            expected[..body.runs.len()],
            "the file surface and the diff colour the same bytes the same way"
        );
    }

    /// Every line has a run list whether or not it has colours, so the render
    /// path indexes without a bounds dance - including for a file whose
    /// extension no grammar claims.
    #[test]
    fn a_file_with_no_grammar_still_has_a_run_list_per_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.xyz");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("write");
        let syntax = crate::diff::FileSyntax::from_theme(&crate::theme::one_dark());

        let FileBody::Text(body) = load_from_disk(&path, Some(syntax)) else {
            panic!("text is text");
        };
        assert_eq!(body.lines.len(), 3);
        assert_eq!(body.runs.len(), 3);
        assert!(body.runs.iter().all(Vec::is_empty), "nothing to colour");
    }

    /// `None` is the monochrome this surface drew for every file until now, and
    /// it still has to line up with the lines.
    #[test]
    fn no_theme_snapshot_means_no_colours_and_no_missing_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.rs");
        std::fs::write(&path, "fn main() {}\n").expect("write");

        let FileBody::Text(body) = load_from_disk(&path, None) else {
            panic!("a .rs file is text");
        };
        assert_eq!(body.runs.len(), body.lines.len());
        assert!(body.runs.iter().all(Vec::is_empty));
    }

    /// A cut file keeps only the runs it can show, and the parse still saw the
    /// whole text - the lines above the cut are coloured by the grammar rather
    /// than by its recovery from a file that stops mid-token.
    #[test]
    fn a_cut_file_keeps_one_run_list_per_shown_line() {
        let syntax = crate::diff::FileSyntax::from_theme(&crate::theme::one_dark());
        let text = "fn a() {}\n".repeat(10);
        let runs = highlight_for(&text, "rs", Some(syntax), 4);
        assert_eq!(runs.len(), 4, "trimmed to what is shown");
        assert!(runs.iter().any(|line| !line.is_empty()));
        // And the other way: a `shown` past the end pads rather than panics.
        let runs = highlight_for(&text, "rs", Some(syntax), 40);
        assert_eq!(runs.len(), 40);
    }

    /// Selecting half a keyword must not lose the keyword's colour, and
    /// selecting across a colour change must not lose the band. That is the
    /// whole job of the merge, and it is the part `with_highlights` cannot do
    /// for us - it walks its input in order and would slice the text twice.
    #[test]
    fn a_selection_over_coloured_code_keeps_both() {
        let line = "let x = 1;";
        let blue = gpui::hsla(0.6, 1.0, 0.5, 1.0);
        let band = gpui::hsla(0.4, 1.0, 0.5, 0.28);
        // `let` is coloured; the selection covers `t x` - one byte of the
        // keyword and two outside it.
        let out = merge_line_styles(line, &[(0..3, blue)], Some(2..5), band);

        // Every piece is inside the line, non-empty, and ascending.
        assert!(
            out.iter()
                .all(|(r, _)| r.start < r.end && r.end <= line.len())
        );
        assert!(out.windows(2).all(|w| w[0].0.end <= w[1].0.start));

        let at = |i: usize| {
            out.iter()
                .find(|(r, _)| r.start <= i && i < r.end)
                .map(|(_, style)| (style.color, style.background_color))
        };
        assert_eq!(at(0), Some((Some(blue), None)), "keyword, unpicked");
        assert_eq!(at(2), Some((Some(blue), Some(band))), "keyword, picked");
        assert_eq!(at(3), Some((None, Some(band))), "picked, uncoloured");
        assert_eq!(at(5), None, "past the selection and past the run");
    }

    /// A piece that neither source has an opinion about is not emitted at all:
    /// a style that changes nothing still cuts the text into another run.
    #[test]
    fn a_line_with_nothing_to_say_produces_no_styles() {
        assert!(merge_line_styles("plain text", &[], None, gpui::red()).is_empty());
    }

    /// The three inputs `with_highlights` debug-asserts on, all of which a
    /// reload can produce by swapping the text under a selection.
    #[test]
    fn ranges_that_would_panic_are_dropped_rather_than_passed_on() {
        let line = "abc";
        let color = gpui::red();
        // Past the end, empty, and inverted.
        let out = merge_line_styles(line, &[(0..99, color), (1..1, color)], Some(9..12), color);
        assert!(
            out.iter()
                .all(|(r, _)| r.end <= line.len() && r.start < r.end)
        );
    }

    /// A multi-byte line: every offset the merge emits has to land on a char
    /// boundary, because slicing between the bytes of a `\u{2192}` panics.
    #[test]
    fn a_selection_inside_a_multibyte_character_is_refused() {
        let line = "a \u{2192} b";
        // 3 is inside the arrow's three bytes.
        assert_eq!(
            clamp_to_line(line, 3, 4),
            (2, 2),
            "walked back to the arrow"
        );
        let out = merge_line_styles(line, &[], Some(3..4), gpui::red());
        assert!(
            out.iter()
                .all(|(r, _)| line.is_char_boundary(r.start) && line.is_char_boundary(r.end))
        );
    }

    #[test]
    fn a_span_covers_the_middle_lines_whole_and_the_ends_partly() {
        let first = "alpha";
        let middle = "beta";
        let last = "gamma";
        let (lo, hi) = ((0usize, 2usize), (2usize, 3usize));
        assert_eq!(picked_span(0, first, lo, hi), Some(2..5), "from the anchor");
        assert_eq!(picked_span(1, middle, lo, hi), Some(0..4), "all of it");
        assert_eq!(picked_span(2, last, lo, hi), Some(0..3), "up to the head");
        assert_eq!(picked_span(3, "delta", lo, hi), None, "past the end");
        // A caret paints nothing: a click that picked no bytes is not a band.
        assert_eq!(picked_span(0, first, (0, 2), (0, 2)), None);
    }

    /// Double click takes an identifier whole and stops where a person would
    /// expect it to. The stopping is the point: `foo.bar` is two words, which
    /// is what makes the gesture useful on code.
    #[test]
    fn a_double_click_takes_the_word_under_it() {
        assert_eq!(word_bounds("let session_id = 1;", 6), Some((4, 14)));
        assert_eq!(word_bounds("let session_id = 1;", 4), Some((4, 14)));
        assert_eq!(word_bounds("let session_id = 1;", 14), Some((4, 14)));
        assert_eq!(word_bounds("foo.bar", 1), Some((0, 3)), "stops at the dot");
        // Unicode's own segmentation answers `(0, 7)` here - it keeps a dot
        // between letters inside a word for abbreviations - which is why this
        // is not `unicode_word_indices`.
        assert_eq!(
            word_bounds("a\u{2192}b", 0),
            Some((0, 1)),
            "stops at the arrow"
        );
        assert_eq!(
            word_bounds("tail", 4),
            Some((0, 4)),
            "a word at the line's end"
        );
        assert_eq!(
            word_bounds("\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} x", 2),
            Some((0, 12)),
            "not an ASCII rule"
        );
        assert_eq!(word_bounds("foo.bar", 5), Some((4, 7)));
        // A caret strictly inside whitespace belongs to no word.
        assert_eq!(word_bounds("a   b", 2), None);
        // But a caret at a word's edge belongs to that word, and a single space
        // offers exactly two caret positions - one at each neighbour's edge.
        // Called out because it reads as an exception and is the same rule:
        // there is no position "inside" a one-character gap.
        assert_eq!(
            word_bounds("ab cd", 2),
            Some((0, 2)),
            "left half of the space"
        );
        assert_eq!(word_bounds("ab cd", 3), Some((3, 5)), "right half");
        assert_eq!(word_bounds("", 0), None);
    }

    #[test]
    fn the_two_ends_of_a_selection_sort_by_line_then_byte() {
        assert_eq!(ordered((2, 1), (0, 9)), ((0, 9), (2, 1)));
        assert_eq!(ordered((1, 9), (1, 2)), ((1, 2), (1, 9)));
        assert_eq!(ordered((1, 2), (1, 2)), ((1, 2), (1, 2)));
    }

    #[test]
    fn copying_a_selection_takes_the_bytes_and_not_the_gutter() {
        let view = printed_view(&["alpha", "beta", "gamma"]);
        // Mid-word on the first line to mid-word on the last.
        let picked = view_with_selection(view, ((0, 2), (2, 3)));
        assert_eq!(picked.selected_text().as_deref(), Some("pha\nbeta\ngam"));
    }

    /// A selection that spans a line break copies the break and nothing else,
    /// which is the honest answer and not an empty string.
    #[test]
    fn a_selection_of_just_a_line_break_copies_it() {
        let view = printed_view(&["alpha", "beta"]);
        let picked = view_with_selection(view, ((0, 5), (1, 0)));
        assert_eq!(picked.selected_text().as_deref(), Some("\n"));
    }

    /// CRLF: `str::lines()` strips the `\r`, and `highlight_lines` splits the
    /// same way, so the per-line offsets the two produce have to agree. A
    /// disagreement would shift every run on every line by one byte and the
    /// `usable` filter would silently drop the tail of each.
    #[test]
    fn a_crlf_file_lines_up_with_its_colour_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("crlf.rs");
        std::fs::write(&path, "fn a() {}\r\nfn b() {}\r\n").expect("write");
        let syntax = crate::diff::FileSyntax::from_theme(&crate::theme::one_dark());

        let FileBody::Text(body) = load_from_disk(&path, Some(syntax)) else {
            panic!("a .rs file is text");
        };
        assert_eq!(body.lines.len(), 2);
        assert!(body.lines.iter().all(|l| !l.contains('\r')), "no stray CR");
        for (line, runs) in body.lines.iter().zip(&body.runs) {
            assert!(
                runs.iter().all(|(r, _)| r.end <= line.len()),
                "a run reaching past its own line means the two splits disagree"
            );
        }
        assert!(
            body.runs.iter().any(|r| !r.is_empty()),
            "and it is coloured"
        );
    }

    #[test]
    fn a_selection_that_picked_no_bytes_is_not_a_selection() {
        let view = printed_view(&["alpha"]);
        let caret = view_with_selection(view, ((0, 2), (0, 2)));
        // `None`, so `\u{2318}C` falls through to the whole file - which is
        // what it does with no pick at all, and a caret is no pick.
        assert_eq!(caret.selected_text(), None);
    }

    fn printed_view(lines: &[&str]) -> FileBody {
        FileBody::Text(TextBody {
            lines: lines
                .iter()
                .map(|l| SharedString::from(l.to_string()))
                .collect(),
            cut_after: None,
            runs: vec![Vec::new(); lines.len()],
            ext: "txt".to_string(),
        })
    }

    /// A `FileView` is a GPUI entity; `selected_text` reads only these two
    /// fields, so the test builds the smallest thing that has them.
    struct Picked {
        body: FileBody,
        text_selection: Option<(TextPoint, TextPoint)>,
    }

    impl Picked {
        fn selected_text(&self) -> Option<String> {
            // Mirrors `FileView::selected_text`, which cannot be called without
            // a window. Kept next to it so a change to one shows up here as a
            // failure rather than as silence.
            let (anchor, head) = self.text_selection?;
            let FileBody::Text(text) = &self.body else {
                return None;
            };
            let (lo, hi) = ordered(anchor, head);
            if lo == hi {
                return None;
            }
            let mut out = String::new();
            for line_idx in lo.0..=hi.0.min(text.lines.len().saturating_sub(1)) {
                let line = text.lines.get(line_idx)?.as_ref();
                let start = if line_idx == lo.0 { lo.1 } else { 0 };
                let end = if line_idx == hi.0 { hi.1 } else { line.len() };
                let (start, end) = clamp_to_line(line, start, end);
                if line_idx > lo.0 {
                    out.push('\n');
                }
                out.push_str(&line[start..end]);
            }
            Some(out)
        }
    }

    fn view_with_selection(body: FileBody, selection: (TextPoint, TextPoint)) -> Picked {
        Picked {
            body,
            text_selection: Some(selection),
        }
    }

    #[test]
    fn the_find_bar_searches_a_printed_file_too() {
        let text = FileBody::Text(TextBody {
            lines: vec!["alpha".into(), "beta".into()],
            cut_after: None,
            runs: vec![Vec::new(); 2],
            ext: String::new(),
        });
        assert_eq!(text.corpus(), "alpha\nbeta\n");
        // Nothing to search in a file there is no viewer for.
        assert!(
            FileBody::Opaque {
                bytes: 4,
                kind: "PNG".into()
            }
            .corpus()
            .is_empty()
        );
    }

    #[test]
    fn a_file_over_the_read_cap_still_gets_an_answer() {
        // The refusal used to reach every kind, which sent the one kind the
        // card exists for - a big binary - to an error pane with no way out.
        let tmp = tempfile::tempdir().expect("tempdir");

        let big_binary = tmp.path().join("huge.bin");
        let mut bytes = vec![b'a'; MAX_INPUT_BYTES + 1];
        bytes[10] = 0;
        write(&big_binary, &bytes);
        match load_from_disk(&big_binary, None) {
            // The size is the file's own, not the head that was read.
            FileBody::Opaque { bytes, .. } => {
                assert_eq!(bytes, (MAX_INPUT_BYTES + 1) as u64)
            }
            other => panic!("expected the card, got {}", body_kind(&other)),
        }

        let big_text = tmp.path().join("huge.log");
        let lines: String = (0..10).map(|n| format!("line {n}\n")).collect();
        let mut filler = vec![b'x'; MAX_INPUT_BYTES + 1];
        filler.splice(0..0, lines.bytes());
        write(&big_text, &filler);
        match load_from_disk(&big_text, None) {
            // Cut even though the head fits the line budget: claiming these
            // are all the lines there are would be false.
            FileBody::Text(text) => assert!(text.cut_after.is_some()),
            other => panic!("expected text, got {}", body_kind(&other)),
        }
    }

    #[test]
    fn a_markdown_file_over_the_cap_still_refuses() {
        // It has to parse the whole document to render any of it, so a head is
        // not an answer here.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("huge.md");
        write(&path, &vec![b'a'; MAX_INPUT_BYTES + 1]);
        assert!(error_of(&load_from_disk(&path, None)).contains("too large"));
    }

    #[test]
    fn the_badge_is_the_extension_uppercased() {
        use std::path::Path;
        assert_eq!(
            extension_badge(Path::new("/a/package.json")).as_deref(),
            Some("JSON")
        );
        assert_eq!(
            extension_badge(Path::new("/a/yarn.lock")).as_deref(),
            Some("LOCK")
        );
        // Nothing to uppercase, and nothing to claim about it.
        assert_eq!(extension_badge(Path::new("/a/Makefile")), None);
        // A version suffix is not a kind, and `3` on a badge says nothing.
        assert_eq!(extension_badge(Path::new("/a/libfoo.so.1.2.3")), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_replacement_is_rejected() {
        // SEC defence: if the watched path becomes a symlink between open
        // and reload, refuse to follow it. Simulates the rename-over attack
        // an adversarial agent could mount in the user's project directory.
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let real_target = tmp.path().join("secret.txt");
        write(&real_target, b"sensitive\n");
        let view_path = tmp.path().join("README.md");
        symlink(&real_target, &view_path).expect("symlink");

        let body = load_from_disk(&view_path, None);
        let msg = error_of(&body);
        assert!(
            msg.contains("symlink"),
            "expected symlink rejection message, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_swapped_in_after_check_is_still_rejected() {
        // CWE-367 regression: the old code stat'd the path, then did a separate
        // symlink-following read. `read_no_follow` collapses both into one
        // O_NOFOLLOW open, so even a symlink that is the *current* final
        // component (the post-swap state) is refused at read time - there is no
        // window where a regular-file stat is paired with a symlink read.
        // Before the fix, the read path followed the symlink and disclosed the
        // target; after it, the open fails with ELOOP and we surface the
        // refusal message.
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret = tmp.path().join("secret.txt");
        write(&secret, b"# TOP SECRET\n");
        let view_path = tmp.path().join("README.md");
        symlink(&secret, &view_path).expect("symlink");

        let body = load_from_disk(&view_path, None);
        let msg = error_of(&body);
        assert!(
            msg.contains("symlink"),
            "expected symlink rejection, got: {msg}"
        );
    }

    #[test]
    fn event_is_relevant_filters_siblings() {
        use std::ffi::OsString;
        let target = OsString::from("README.md");
        let make = |path: &str| -> notify::Result<notify::Event> {
            Ok(notify::Event {
                kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
                paths: vec![PathBuf::from(path)],
                attrs: Default::default(),
            })
        };
        assert!(event_is_relevant(&make("/x/README.md"), &target));
        assert!(!event_is_relevant(&make("/x/other.md"), &target));
        // Errors are ignored.
        assert!(!event_is_relevant(
            &Err(notify::Error::generic("boom")),
            &target
        ));
    }

    #[test]
    fn harvest_text_concatenates_paragraph_spans_with_inline_styles() {
        // Verifies the search corpus contains the visible text of a
        // formatted paragraph. pulldown-cmark preserves inter-span spaces in
        // its Text events, so the corpus reads "this is bold text" as the
        // user sees it (regression guard).
        let nodes = parse_with_limit("this is **bold** text\n").expect("parse");
        let corpus = harvest_text(&nodes);
        assert!(
            corpus.contains("this is bold text"),
            "corpus missing space-joined text: {:?}",
            corpus
        );
    }

    #[test]
    fn harvest_text_includes_code_block_content() {
        let nodes = parse_with_limit("```rust\nfn main() {}\n```\n").expect("parse");
        let corpus = harvest_text(&nodes);
        assert!(corpus.contains("fn main() {}"));
    }

    #[test]
    fn harvest_text_walks_nested_lists() {
        let src = "- top1\n  - nested-a\n- top2\n";
        let nodes = parse_with_limit(src).expect("parse");
        let corpus = harvest_text(&nodes);
        for needle in &["top1", "nested-a", "top2"] {
            assert!(
                corpus.contains(needle),
                "missing {} in {:?}",
                needle,
                corpus
            );
        }
    }

    #[test]
    fn truncate_for_clipboard_short_input_unchanged() {
        let small = "small";
        assert_eq!(truncate_for_clipboard(small), "small");
    }

    #[test]
    fn truncate_for_clipboard_caps_large_payload() {
        let huge: String = std::iter::repeat_n('a', COPY_MAX_BYTES + 100).collect();
        let bounded = truncate_for_clipboard(&huge);
        assert!(bounded.len() <= COPY_MAX_BYTES + "\n…[truncated]".len());
        assert!(bounded.ends_with("[truncated]"));
    }

    #[test]
    fn truncate_for_clipboard_respects_utf8_boundaries() {
        // Build a string whose byte at COPY_MAX_BYTES falls inside a 3-byte
        // codepoint. The truncator must walk back to the boundary rather
        // than splitting the codepoint (which would panic the slice).
        let mut s = String::with_capacity(COPY_MAX_BYTES + 8);
        while s.len() < COPY_MAX_BYTES - 2 {
            s.push('a');
        }
        // 3-byte UTF-8 codepoint that straddles the cap.
        s.push('日');
        while s.len() < COPY_MAX_BYTES + 32 {
            s.push('a');
        }
        let _ = truncate_for_clipboard(&s);
    }
}
