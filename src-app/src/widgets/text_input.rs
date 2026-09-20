//! Cursor-aware single-line text input widget for Splitlane.
//!
//! Adapted from GPUI's upstream `examples/input.rs` (pinned via the Zed git
//! dep) with three goals: (1) splitlane theme colours instead of the demo's
//! hardcoded greys, (2) a caller-supplied styled wrapper (so modal / settings
//! contexts can control padding, border, font), (3) cross-platform ctrl/cmd
//! clipboard bindings (Linux-first but macOS-correct).
//!
//! Supports: mouse click to position cursor, click-drag to select, shift+click
//! to extend selection, arrow keys (+ shift to select), Home / End, Backspace /
//! Delete, Ctrl/Cmd+A / C / V / X, IME composition (CJK, dead keys).
//!
//! # Multi-line mode
//!
//! [`TextInput::multiline`] makes the field hold `\n`: the element lays out one
//! shaped line per logical line and grows its height to match, paste keeps its
//! line breaks instead of flattening them, and Home / End move within the
//! current line rather than the whole value.
//!
//! What multi-line mode deliberately does **not** add is a key that inserts the
//! break. There is no `enter` binding in the `TextInput` context and there must
//! not be one: in the agent composer ⏎ sends and ⇧⏎ inserts, and a field that
//! claimed `enter` for itself would take the send key away from its owner. The
//! owner calls [`TextInput::insert_newline`] and [`TextInput::move_line`] from
//! its own key handler, which is also what lets ↑↓ drive an autocomplete
//! popover when one is open and the caret when one is not.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId, Hsla, IntoElement,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Render, ShapedLine, SharedString, Style, Styled, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, hsla, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        TextInputPaste,
        TextInputCut,
        TextInputCopy,
    ]
);

/// Register the keybindings that drive every `TextInput` instance.
/// Must be called once during app startup, **after** GPUI's App has been
/// created, and before any `TextInput` receives keyboard input.
pub fn register_keybindings(cx: &mut App) {
    // Platform-agnostic bindings (arrows, edit keys, selection extension).
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
    ]);

    // Primary-modifier clipboard bindings. macOS uses Cmd, Linux/Windows use
    // Ctrl - matching the platform convention (and OS text-input expectations).
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
        KeyBinding::new("cmd-c", TextInputCopy, Some("TextInput")),
        KeyBinding::new("cmd-v", TextInputPaste, Some("TextInput")),
        KeyBinding::new("cmd-x", TextInputCut, Some("TextInput")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("TextInput")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
        KeyBinding::new("ctrl-c", TextInputCopy, Some("TextInput")),
        KeyBinding::new("ctrl-v", TextInputPaste, Some("TextInput")),
        KeyBinding::new("ctrl-x", TextInputCut, Some("TextInput")),
    ]);
}

// ---------------------------------------------------------------------------
// TextInput entity
// ---------------------------------------------------------------------------

pub struct TextInput {
    pub focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// One shaped line per logical line, with the byte range of the content it
    /// came from. A single-line field is the one-entry case, not a second path.
    last_lines: Vec<LaidOutLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_line_height: Pixels,
    is_selecting: bool,
    multiline: bool,
    /// The ink this field shapes its content in, when the caller has an opinion.
    /// `None` takes `ui.text` - see the note in `render`.
    text_color: Option<Hsla>,
}

/// A shaped line and the slice of the content it shaped.
struct LaidOutLine {
    line: ShapedLine,
    range: Range<usize>,
}

impl TextInput {
    /// Create a new input with an initial value and placeholder.
    pub fn new(
        initial: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let content: SharedString = initial.into();
        let cursor = content.len();
        Self {
            focus_handle: cx.focus_handle(),
            content,
            placeholder: placeholder.into(),
            selected_range: cursor..cursor,
            selection_reversed: false,
            marked_range: None,
            last_lines: Vec::new(),
            last_bounds: None,
            last_line_height: px(0.),
            is_selecting: false,
            multiline: false,
            text_color: None,
        }
    }

    /// Shape this field's content in `color` rather than in `ui.text`.
    ///
    /// The escape hatch for a field mounted on something that is not the app's
    /// ordinary ground - an accent fill, a danger surface - where `ui.text` is
    /// the wrong ink. Nothing needs it yet; it exists so that the default in
    /// `render` is a floor rather than a ceiling, which is the difference
    /// between one answer and one rule.
    #[allow(dead_code)]
    pub fn with_text_color(mut self, color: Hsla) -> Self {
        self.text_color = Some(color);
        self
    }

    /// Turn on multi-line editing. See the module docs for what it does and does
    /// not bind.
    ///
    /// The composer was the only caller of the multi-line half of this widget
    /// and of the caret-aware setters, and it went with the rendered face. The
    /// methods stay, hence the `dead_code` allowances: `multiline` is a mode of
    /// the element itself - it decides how `render` shapes lines, how selection
    /// is clamped and how paste is sanitized - so unpicking it would be surgery
    /// on a field ten other screens use, for no gain. The next thing that needs
    /// a text area finds it here rather than writing it twice.
    #[allow(dead_code)]
    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    /// Insert a line break at the caret. Only meaningful in multi-line mode; a
    /// single-line field would render the `\n` as a stray glyph, so it is a
    /// no-op there rather than a corruption.
    #[allow(dead_code)]
    pub fn insert_newline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            return;
        }
        self.replace_text_in_range(None, "\n", window, cx);
    }

    /// Move the caret one line up (`-1`) or down (`+1`), keeping the column.
    ///
    /// Returns false when there is no line that way, which is what lets the
    /// owner hand the keystroke on to whatever else wants ↑↓.
    #[allow(dead_code)]
    pub fn move_line(&mut self, delta: i32, cx: &mut Context<Self>) -> bool {
        if !self.multiline {
            return false;
        }
        let (line, column) = self.line_and_column(self.cursor_offset());
        let target = line as i32 + delta;
        if target < 0 {
            return false;
        }
        let starts = self.line_starts();
        let target = target as usize;
        if target >= starts.len() {
            return false;
        }
        let range = self.line_range(target);
        let width = range.end - range.start;
        let offset = range.start + column.min(width);
        self.move_to(self.clamp_to_boundary(offset), cx);
        true
    }

    /// Byte offsets at which each logical line starts. Never empty.
    fn line_starts(&self) -> Vec<usize> {
        line_starts(&self.content, self.multiline)
    }

    /// Byte range of line `index`, excluding its terminating newline.
    fn line_range(&self, index: usize) -> Range<usize> {
        line_range(&self.content, self.multiline, index)
    }

    /// Which line an offset falls on, and how many bytes into it.
    fn line_and_column(&self, offset: usize) -> (usize, usize) {
        line_and_column(&self.content, self.multiline, offset)
    }

    /// Snap an offset onto the nearest grapheme boundary at or before it, so a
    /// column carried between lines cannot land inside a character.
    #[allow(dead_code)]
    fn clamp_to_boundary(&self, offset: usize) -> usize {
        let offset = offset.min(self.content.len());
        if self.content.is_char_boundary(offset) {
            return offset;
        }
        self.previous_boundary(offset)
    }

    /// Current content as an owned `String`.
    pub fn value(&self) -> String {
        self.content.to_string()
    }

    /// Replace the entire input value from app code and move the cursor to
    /// the end. Clears IME composition state because the marked bytes no
    /// longer describe the new content.
    pub fn set_value(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = value.into();
        let cursor = self.content.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.is_selecting = false;
        self.last_lines.clear();
        self.last_bounds = None;
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_value(SharedString::default(), cx);
    }

    /// Change the placeholder shown while the field is empty.
    #[allow(dead_code)]
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder == placeholder {
            return;
        }
        self.placeholder = placeholder;
        cx.notify();
    }

    /// Byte offset of the caret. Owners that complete words need to know where
    /// the word ends, and "the end of the value" is only right for a field
    /// nobody ever clicks into the middle of.
    #[allow(dead_code)]
    pub fn cursor(&self) -> usize {
        self.cursor_offset()
    }

    /// Replace the value and put the caret at `cursor`.
    ///
    /// [`Self::set_value`] jumps to the end, which is right for a field being
    /// filled from app state and wrong for a completion inserted mid-sentence -
    /// there the caret belongs just after what was inserted.
    #[allow(dead_code)]
    pub fn set_value_with_cursor(
        &mut self,
        value: impl Into<SharedString>,
        cursor: usize,
        cx: &mut Context<Self>,
    ) {
        self.set_value(value, cx);
        let cursor = self.clamp_to_boundary(cursor);
        self.selected_range = cursor..cursor;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let (line, _) = self.line_and_column(self.cursor_offset());
        let start = if self.multiline {
            self.line_range(line).start
        } else {
            0
        };
        self.move_to(start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let (line, _) = self.line_and_column(self.cursor_offset());
        let end = if self.multiline {
            self.line_range(line).end
        } else {
            self.content.len()
        };
        self.move_to(end, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &TextInputPaste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // A single-line input coerces newlines to spaces. Collapse
            // CRLF to one space first, then any lone CR/LF, so a Windows-style
            // paste doesn't leave a stray `\r` that snaps the cursor to column
            // 0 and visually corrupts the field.
            //
            // A multi-line field keeps the breaks - pasting a stack trace or a
            // block of code into the composer is the point - but still folds
            // CRLF and a lone CR down to `\n`, because a `\r` is not a line the
            // layout can see and would paint as a stray glyph.
            let sanitized = if self.multiline {
                text.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                text.replace("\r\n", " ").replace(['\r', '\n'], " ")
            };
            self.replace_text_in_range(None, &sanitized, window, cx);
        }
    }

    fn copy(&mut self, _: &TextInputCopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &TextInputCut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.last_bounds.as_ref() else {
            return 0;
        };
        if self.last_lines.is_empty() {
            return 0;
        }
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        let height = self.last_line_height;
        let row = if height > px(0.) {
            (f32::from(position.y - bounds.top()) / f32::from(height)) as usize
        } else {
            0
        };
        let row = row.min(self.last_lines.len() - 1);
        let laid_out = &self.last_lines[row];
        laid_out.range.start
            + laid_out
                .line
                .closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn byte_offset_from_utf16_in_text(text: &str, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in text.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn byte_range_from_utf16_in_text(text: &str, range_utf16: &Range<usize>) -> Range<usize> {
        Self::byte_offset_from_utf16_in_text(text, range_utf16.start)
            ..Self::byte_offset_from_utf16_in_text(text, range_utf16.end)
    }

    fn replacement_range_from_utf16(&self, range_utf16: Option<&Range<usize>>) -> Range<usize> {
        match (self.marked_range.as_ref(), range_utf16) {
            (Some(marked_range), Some(range_utf16)) => {
                let marked_text = &self.content[marked_range.clone()];
                let relative = Self::byte_range_from_utf16_in_text(marked_text, range_utf16);
                marked_range.start + relative.start..marked_range.start + relative.end
            }
            (_, Some(range_utf16)) => self.range_from_utf16(range_utf16),
            (Some(marked_range), None) => marked_range.clone(),
            (None, None) => self.selected_range.clone(),
        }
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.replacement_range_from_utf16(range_utf16.as_ref());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.replacement_range_from_utf16(range_utf16.as_ref());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| Self::byte_range_from_utf16_in_text(new_text, range_utf16))
            .map(|new_range| range.start + new_range.start..range.start + new_range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;

        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        // The IME wants one rectangle. On a wrapped composition it gets the
        // line the composition starts on, which is where the candidate window
        // belongs anyway.
        let row = self.last_lines.iter().position(|laid_out| {
            laid_out.range.start <= range.start && range.start <= laid_out.range.end
        })?;
        let laid_out = &self.last_lines[row];
        let top = bounds.top() + self.last_line_height * (row as f32);
        let start = range.start.saturating_sub(laid_out.range.start);
        let end = range
            .end
            .min(laid_out.range.end)
            .saturating_sub(laid_out.range.start);
        Some(Bounds::from_corners(
            point(bounds.left() + laid_out.line.x_for_index(start), top),
            point(
                bounds.left() + laid_out.line.x_for_index(end),
                top + self.last_line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        // An empty field lays out the *placeholder* ("Filter files…"),
        // so the shaped text legitimately differs from the content. The old
        // `assert_eq!` turned an OS-driven IME/hit-test on an empty field into a
        // SIGABRT. Bail gracefully instead.
        if self.content.is_empty() {
            return None;
        }
        let height = self.last_line_height;
        let row = if height > px(0.) {
            (f32::from(line_point.y) / f32::from(height)) as usize
        } else {
            0
        };
        let laid_out = self
            .last_lines
            .get(row.min(self.last_lines.len().max(1) - 1))?;
        if laid_out.line.text != self.content[laid_out.range.clone()] {
            return None;
        }
        let utf8_index = laid_out.line.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(laid_out.range.start + utf8_index))
    }
}

// ---------------------------------------------------------------------------
// Low-level element - shapes the line and paints text + caret + selection.
// ---------------------------------------------------------------------------

struct TextElement {
    input: Entity<TextInput>,
    /// Caret colour, usually `ui.accent`.
    caret_color: Hsla,
    /// Selection highlight colour, usually `ui.accent` at low alpha.
    selection_color: Hsla,
    /// Placeholder text colour.
    placeholder_color: Hsla,
}

struct PrepaintState {
    /// One entry per logical line, in order.
    lines: Vec<LaidOutLine>,
    line_height: Pixels,
    cursor: Option<PaintQuad>,
    /// One quad per line the selection touches - a selection spanning three
    /// lines is three rectangles, not one box around all of them.
    selection: Vec<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        // The field is as tall as it has lines. A single-line input is the
        // one-line case of that, so there is no second branch to keep in sync.
        let lines = self.input.read(cx).line_starts().len().max(1);
        style.size.height = (window.line_height() * (lines as f32)).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let marked_range = input.marked_range.clone();
        let placeholder = input.placeholder.clone();
        let style = window.text_style();
        let line_height = window.line_height();
        let font_size = style.font_size.to_pixels(window.rem_size());

        // An empty field lays out the placeholder as its one line, and that line
        // covers the empty range `0..0` so the caret still finds a home.
        let showing_placeholder = content.is_empty();
        let (display, ranges): (SharedString, Vec<Range<usize>>) = if showing_placeholder {
            (placeholder, line_spans(""))
        } else {
            let ranges = line_spans(&content);
            (content.clone(), ranges)
        };
        let text_color = if showing_placeholder {
            self.placeholder_color
        } else {
            style.color
        };

        let mut lines: Vec<LaidOutLine> = Vec::with_capacity(ranges.len());
        for range in &ranges {
            let slice: SharedString = if showing_placeholder {
                display.clone()
            } else {
                display[range.clone()].to_string().into()
            };
            let base = TextRun {
                len: slice.len(),
                font: style.font(),
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = if showing_placeholder {
                vec![base]
            } else {
                runs_for(range, marked_range.as_ref(), base)
            };
            let line = window
                .text_system()
                .shape_line(slice, font_size, &runs, None);
            lines.push(LaidOutLine {
                line,
                range: range.clone(),
            });
        }

        let row_top = |row: usize| bounds.top() + line_height * (row as f32);
        let locate = |offset: usize| -> (usize, Pixels) {
            for (row, laid_out) in lines.iter().enumerate() {
                if offset <= laid_out.range.end {
                    let local = offset.saturating_sub(laid_out.range.start);
                    return (row, laid_out.line.x_for_index(local));
                }
            }
            let row = lines.len().saturating_sub(1);
            let x = lines
                .last()
                .map(|laid_out| laid_out.line.x_for_index(laid_out.range.len()))
                .unwrap_or_default();
            (row, x)
        };

        let (cursor, selection) = if selected_range.is_empty() {
            let (row, x) = locate(cursor);
            (
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + x, row_top(row)),
                        // 1px hairline caret (Codex-quiet) - 2px read as a
                        // block on small input text.
                        size(px(1.), line_height),
                    ),
                    self.caret_color,
                )),
                Vec::new(),
            )
        } else {
            let mut quads = Vec::new();
            for (row, laid_out) in lines.iter().enumerate() {
                let start = selected_range.start.max(laid_out.range.start);
                let end = selected_range.end.min(laid_out.range.end);
                if start > end {
                    continue;
                }
                if start == end && !(laid_out.range.start..=laid_out.range.end).contains(&start) {
                    continue;
                }
                let left = laid_out.line.x_for_index(start - laid_out.range.start);
                let right = laid_out.line.x_for_index(end - laid_out.range.start);
                if right <= left {
                    continue;
                }
                quads.push(fill(
                    Bounds::from_corners(
                        point(bounds.left() + left, row_top(row)),
                        point(bounds.left() + right, row_top(row) + line_height),
                    ),
                    self.selection_color,
                ));
            }
            (None, quads)
        };

        PrepaintState {
            lines,
            line_height,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in std::mem::take(&mut prepaint.selection) {
            window.paint_quad(selection);
        }
        let lines = std::mem::take(&mut prepaint.lines);
        let line_height = prepaint.line_height;
        for (row, laid_out) in lines.iter().enumerate() {
            laid_out
                .line
                .paint(
                    point(
                        bounds.origin.x,
                        bounds.origin.y + line_height * (row as f32),
                    ),
                    line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
        }

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_lines = lines;
            input.last_bounds = Some(bounds);
            input.last_line_height = line_height;
        });
    }
}

// ---------------------------------------------------------------------------
// Render impl - just the key/mouse hit area; caller styles the outer box.
// ---------------------------------------------------------------------------

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        // Selection highlight: accent colour at low alpha. GPUI's `Hsla`
        // literal copy + alpha override keeps the hue aligned with the
        // active theme (so it stays coherent across One Dark / Splitlane Light).
        let selection = hsla(ui.accent.h, ui.accent.s, ui.accent.l, 0.28);

        div()
            .w_full()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            // **The field's ink has one home, and this is it.** The element
            // below shapes its content in `window.text_style().color` - what
            // the mount point happens to have inherited - so a wrapper that
            // states no colour hands the query GPUI's default, which on a dark
            // pane is very nearly the pane. That is the `svg()` failure in a
            // second place: the field focuses, filters, and answers every key,
            // and only the seeing of it is missing, which is why the pane
            // launcher shipped with an invisible query for four rounds.
            //
            // Every mount in this app wanted `ui.text` and ten of the eleven
            // said so; stating it here is what stops the eleventh from
            // existing. Size and font stay the caller's - those genuinely
            // differ (the find bar is mono, the palette is not) and a wrong
            // one is visible on sight.
            //
            // It is a **floor and not a ceiling**: this div is inside the
            // caller's, so what it states here wins over anything the mount
            // point inherits, and a field on a coloured surface that needed
            // `on_accent` would have been silently overridden - the same
            // invisible-by-construction failure this exists to end, pointing
            // the other way. [`TextInput::with_text_color`] is that door. No
            // caller needs it today; it is here so that the day one does, the
            // answer is a method rather than a puzzle.
            .text_color(self.text_color.unwrap_or(ui.text))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement {
                input: cx.entity(),
                // White caret (ui.text), not accent - the accent stays reserved
                // for status; a blue caret shouted in every input.
                caret_color: ui.text,
                selection_color: selection,
                placeholder_color: ui.muted,
            })
    }
}

/// The byte span of every logical line, newlines excluded. Never empty.
fn line_spans(content: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for (idx, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            spans.push(start..idx);
            start = idx + 1;
        }
    }
    spans.push(start..content.len());
    spans
}

/// Byte offsets at which each logical line starts. Never empty; a single-line
/// field is always `[0]`, which is what keeps the layout one code path.
fn line_starts(content: &str, multiline: bool) -> Vec<usize> {
    let mut starts = vec![0usize];
    if multiline {
        for (idx, byte) in content.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(idx + 1);
            }
        }
    }
    starts
}

/// Byte range of one logical line, excluding its terminating newline.
fn line_range(content: &str, multiline: bool, index: usize) -> Range<usize> {
    let starts = line_starts(content, multiline);
    let start = starts.get(index).copied().unwrap_or(content.len());
    let end = starts
        .get(index + 1)
        .map(|next| next.saturating_sub(1))
        .unwrap_or(content.len());
    start..end.max(start)
}

/// Which line an offset falls on, and how many bytes into it.
fn line_and_column(content: &str, multiline: bool, offset: usize) -> (usize, usize) {
    let starts = line_starts(content, multiline);
    let line = starts
        .iter()
        .rposition(|start| *start <= offset)
        .unwrap_or(0);
    (line, offset - starts[line])
}

/// Split one line into text runs, underlining whatever part of it the IME has
/// marked.
///
/// The marked range is in content offsets and a composition can straddle a line
/// break, so it is intersected with this line rather than assumed to sit inside
/// it.
fn runs_for(line: &Range<usize>, marked: Option<&Range<usize>>, base: TextRun) -> Vec<TextRun> {
    let Some(marked) = marked else {
        return vec![base];
    };
    let start = marked.start.max(line.start);
    let end = marked.end.min(line.end);
    if start >= end {
        return vec![base];
    }
    let underline = Some(UnderlineStyle {
        color: Some(base.color),
        thickness: px(1.0),
        wavy: false,
    });
    vec![
        TextRun {
            len: start - line.start,
            ..base.clone()
        },
        TextRun {
            len: end - start,
            underline,
            ..base.clone()
        },
        TextRun {
            len: line.end - end,
            ..base
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{TextInput, line_and_column, line_range, line_starts};

    /// A single-line field has exactly one line no matter what is in it: the
    /// paste path already folded newlines away, and a stray one must not make
    /// the element grow a second row under a fixed-height box.
    #[test]
    fn a_single_line_field_is_always_one_line() {
        assert_eq!(line_starts("a\nb\nc", false), vec![0]);
        assert_eq!(line_range("a\nb\nc", false, 0), 0..5);
    }

    #[test]
    fn line_starts_follow_every_break_including_a_trailing_one() {
        assert_eq!(line_starts("ab\ncd", true), vec![0, 3]);
        // A trailing newline opens an empty last line - the caret has to be able
        // to sit on it after ⇧⏎.
        assert_eq!(line_starts("ab\n", true), vec![0, 3]);
        assert_eq!(line_range("ab\n", true, 1), 3..3);
    }

    #[test]
    fn a_line_range_excludes_the_break_that_ends_it() {
        assert_eq!(line_range("ab\ncd", true, 0), 0..2);
        assert_eq!(line_range("ab\ncd", true, 1), 3..5);
    }

    #[test]
    fn a_column_is_measured_from_the_line_not_the_value() {
        assert_eq!(line_and_column("ab\ncd", true, 4), (1, 1));
        assert_eq!(line_and_column("ab\ncd", true, 3), (1, 0));
        assert_eq!(line_and_column("ab\ncd", true, 2), (0, 2));
    }

    #[test]
    fn utf16_range_conversion_handles_surrogate_pairs() {
        let text = "a😀b";

        assert_eq!(
            TextInput::byte_range_from_utf16_in_text(text, &(1..3)),
            1..5
        );
    }

    #[test]
    fn utf16_range_conversion_clamps_to_text_end() {
        let text = "é";

        assert_eq!(
            TextInput::byte_range_from_utf16_in_text(text, &(0..99)),
            0..2
        );
    }
}
