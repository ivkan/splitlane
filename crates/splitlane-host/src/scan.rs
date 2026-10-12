//! What a stretch of terminal output leaves switched on, found without a
//! terminal.
//!
//! The log is a ring, and a client that attaches after the ring wrapped
//! replays output from somewhere in the middle. Two things go wrong there
//! unless the host prepares for them. The replay may begin inside an escape
//! sequence, a UTF-8 character or a synchronized update. And it has lost
//! whatever the program switched on once and never again: the alternate
//! screen, bracketed paste, mouse reporting, the keyboard protocol.
//!
//! The [`Scanner`] reads every byte the session prints and keeps both
//! answers. It knows where the stream may be cut, and it knows which modes
//! are in effect there, as [`Modes`]. [`Modes::preamble`] turns the second
//! into bytes that put any emulator into that state, whatever state it was
//! in. It does not restore what is on the screen: that is the program's to
//! repaint.
//!
//! The scanner is deliberately not an emulator and belongs to no emulator.
//! It follows the byte-level grammar every one of them shares, and where
//! emulators disagree about what a sequence does it records the sequence
//! rather than an interpretation of it; see [`Ops`].
//!
//! It runs on the thread that reads the PTY, on every byte, so it allocates
//! only when a mode actually changes and a snapshot is asked for.

use std::sync::Arc;

/// Longest window title kept. A title is repeated in every preamble, so one
/// megabyte of it must not be.
pub const TITLE_MAX: usize = 1024;

/// Parameters of one control sequence that are looked at. No sequence the
/// scanner acts on has more.
const MAX_PARAMS: usize = 16;

/// Entries kept of a keyboard-protocol stack: the depth of the protocol's
/// own implementation. Pushing past it drops the oldest entry there too.
const KITTY_DEPTH: usize = 8;

/// How long a synchronized update may stay open before it is taken to have
/// been abandoned. Emulators give up on one too, after this many bytes or
/// after a fraction of a second; a program that died inside one would
/// otherwise leave the rest of the session without a single place to cut.
const SYNC_MAX_BYTES: usize = 2 * 1024 * 1024;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const BEL: u8 = 0x07;

/// Private modes that are plain switches: nothing else changes them and
/// they change nothing else. `(number, bit, on at start)`.
const SWITCHES: [(u16, u8, bool); 6] = [
    (1, 1 << 0, false),    // application cursor keys
    (7, 1 << 6, true),     // wrap at the right margin
    (25, 1 << 1, true),    // cursor visible
    (1004, 1 << 2, false), // focus reporting
    (2004, 1 << 3, false), // bracketed paste
    (2031, 1 << 4, false), // colour scheme reporting
];
const KEYPAD_APPLICATION: u8 = 1 << 5;
const SWITCHES_AT_START: u8 = 1 << 1 | 1 << 6;

/// The three spellings of the alternate screen.
const SCREEN_MODES: [u16; 3] = [47, 1047, 1049];
/// What the mouse reports.
const MOUSE_MODES: [u16; 3] = [1000, 1002, 1003];
/// How the mouse report is encoded.
const MOUSE_ENCODINGS: [u16; 4] = [1005, 1006, 1015, 1016];

/// The last time each of a few related sequences was seen, oldest first.
///
/// The modes of a group act on one another, and emulators disagree about
/// how. Setting one mouse mode clears the other two in one emulator and
/// leaves them alone in the next; `47` and `1047` switch screens in one and
/// are ignored by another. So no set of flags describes the group for all
/// of them. What does is the sequences themselves: each one assigns fixed
/// values to some of the group's state, so replaying only the last
/// occurrence of every distinct sequence, in the order those occurrences
/// came, ends in the state the whole stream ended in. That holds for any
/// emulator, without knowing what it does with each sequence.
///
/// An entry is the mode's index in its group, doubled, plus one for "set".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ops {
    list: [u8; 8],
    len: u8,
}

impl Ops {
    fn seen(mut self, index: usize, set: bool) -> Self {
        let op = (index as u8) << 1 | u8::from(set);
        let len = self.len as usize;
        if let Some(at) = self.list[..len].iter().position(|have| *have == op) {
            self.list.copy_within(at + 1..len, at);
            self.list[len - 1] = op;
        } else if len < self.list.len() {
            self.list[len] = op;
            self.len += 1;
        }
        self
    }

    fn iter(&self) -> impl Iterator<Item = (usize, bool)> + '_ {
        self.list[..self.len as usize]
            .iter()
            .map(|op| ((op >> 1) as usize, op & 1 == 1))
    }
}

/// The keyboard-protocol flags of one screen: a stack whose top is in
/// effect. Follows the protocol's reference implementation, in which
/// setting flags rewrites the top entry, and setting them on an empty stack
/// makes the first entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KittyStack {
    entries: [u8; KITTY_DEPTH],
    len: u8,
}

impl KittyStack {
    const FLAGS: u16 = 0b1_1111;

    fn top(&self) -> u8 {
        match self.len {
            0 => 0,
            len => self.entries[len as usize - 1],
        }
    }

    fn pushed(mut self, flags: u16) -> Self {
        if self.len as usize == KITTY_DEPTH {
            self.entries.copy_within(1.., 0);
            self.len -= 1;
        }
        self.entries[self.len as usize] = (flags & Self::FLAGS) as u8;
        self.len += 1;
        self
    }

    fn popped(mut self, count: u16) -> Self {
        self.len = self.len.saturating_sub(count.min(KITTY_DEPTH as u16) as u8);
        // Cleared, so that two stacks holding the same entries are equal.
        self.entries[self.len as usize..].fill(0);
        self
    }

    fn set(mut self, flags: u16, how: u16) -> Self {
        let flags = (flags & Self::FLAGS) as u8;
        let next = match how {
            2 => self.top() | flags,
            3 => self.top() & !flags,
            _ => flags,
        };
        if self.len == 0 {
            return self.pushed(next.into());
        }
        self.entries[self.len as usize - 1] = next;
        self
    }

    fn as_slice(&self) -> &[u8] {
        &self.entries[..self.len as usize]
    }
}

/// The terminal modes in effect at one point of a session's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Modes {
    switches: u8,
    /// Alternate scroll, mode 1007. Emulators differ in whether it starts
    /// on, so until the program touches it there is nothing to restore.
    alternate_scroll: Option<bool>,
    screen: Ops,
    /// Whether the program is on the alternate screen, counting all three
    /// spellings. Decides which keyboard stack a sequence belongs to.
    alt_screen: bool,
    mouse: Ops,
    mouse_encoding: Ops,
    /// Keyboard-protocol stacks of the main and the alternate screen.
    kitty: [KittyStack; 2],
    /// Parameter of the last cursor-shape sequence; 0 is the default shape.
    cursor_shape: u8,
    title: Option<Arc<[u8]>>,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            switches: SWITCHES_AT_START,
            alternate_scroll: None,
            screen: Ops::default(),
            alt_screen: false,
            mouse: Ops::default(),
            mouse_encoding: Ops::default(),
            kitty: [KittyStack::default(); 2],
            cursor_shape: 0,
            title: None,
        }
    }
}

impl Modes {
    pub fn alt_screen(&self) -> bool {
        self.alt_screen
    }

    /// Whether a plain private mode is set. `None` for a mode that is not
    /// tracked as a plain switch.
    pub fn switch(&self, mode: u16) -> Option<bool> {
        if mode == 1007 {
            return self.alternate_scroll;
        }
        SWITCHES
            .iter()
            .find(|(number, ..)| *number == mode)
            .map(|(_, bit, _)| self.switches & bit != 0)
    }

    pub fn keypad_application(&self) -> bool {
        self.switches & KEYPAD_APPLICATION != 0
    }

    /// Parameter of the last cursor-shape sequence; 0 is the default shape.
    pub fn cursor_shape(&self) -> u8 {
        self.cursor_shape
    }

    pub fn title(&self) -> Option<&[u8]> {
        self.title.as_deref()
    }

    /// Keyboard-protocol stack of the main (`false`) or alternate (`true`)
    /// screen, oldest entry first.
    pub fn keyboard_stack(&self, alt_screen: bool) -> &[u8] {
        self.kitty[usize::from(alt_screen)].as_slice()
    }

    /// Bytes that put an emulator into these modes, whatever state it was
    /// in before.
    ///
    /// Nothing is left to the emulator's current state, because the same
    /// bytes resynchronise a client that lost a stretch of output in the
    /// middle of a session, and that client's emulator may be anywhere: so
    /// every mode gets an explicit set or reset, and a mode the program has
    /// switched off is switched off here too.
    ///
    /// The screen's content is not restored and the alternate screen comes
    /// out blank; the program repaints it when it is asked to redraw.
    pub fn preamble(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        // The emulator may be in the middle of anything: a sequence that
        // was cut, a string, a synchronized update it is still buffering.
        out.push(CAN);
        out.extend_from_slice(b"\x1b[?2026l\x1b[0m");

        // Each screen has a keyboard stack of its own, and a stack can only
        // be rebuilt from the screen it belongs to. So: to the main screen
        // by every spelling, across to the alternate one and back, and only
        // then to wherever the program is.
        for mode in SCREEN_MODES {
            private_mode(&mut out, mode, false);
        }
        private_mode(&mut out, 1049, true);
        keyboard_stack(&mut out, &self.kitty[1]);
        private_mode(&mut out, 1049, false);
        keyboard_stack(&mut out, &self.kitty[0]);
        for (index, set) in self.screen.iter() {
            private_mode(&mut out, SCREEN_MODES[index], set);
        }

        for (mode, bit, _) in SWITCHES {
            private_mode(&mut out, mode, self.switches & bit != 0);
        }
        if let Some(set) = self.alternate_scroll {
            private_mode(&mut out, 1007, set);
        }
        for (modes, ops) in [
            (&MOUSE_MODES[..], &self.mouse),
            (&MOUSE_ENCODINGS[..], &self.mouse_encoding),
        ] {
            for mode in modes {
                private_mode(&mut out, *mode, false);
            }
            for (index, set) in ops.iter() {
                private_mode(&mut out, modes[index], set);
            }
        }
        out.extend_from_slice(if self.keypad_application() {
            b"\x1b="
        } else {
            b"\x1b>"
        });
        out.extend_from_slice(format!("\x1b[{} q", self.cursor_shape).as_bytes());
        if let Some(title) = &self.title {
            out.extend_from_slice(b"\x1b]2;");
            out.extend_from_slice(title);
            out.extend_from_slice(b"\x1b\\");
        }
        out
    }
}

fn private_mode(out: &mut Vec<u8>, mode: u16, set: bool) {
    out.extend_from_slice(format!("\x1b[?{mode}{}", if set { 'h' } else { 'l' }).as_bytes());
}

fn keyboard_stack(out: &mut Vec<u8>, stack: &KittyStack) {
    // More than any stack holds, which every implementation reads as "all".
    out.extend_from_slice(b"\x1b[<99u");
    for flags in stack.as_slice() {
        out.extend_from_slice(format!("\x1b[>{flags}u").as_bytes());
    }
}

/// Where the parser is between two bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    /// Parameters of a control sequence.
    Csi,
    CsiIntermediate,
    /// A control sequence that is malformed and runs to its final byte
    /// without doing anything.
    CsiIgnore,
    /// The number that opens an operating system command.
    OscNumber,
    /// The text of one that sets the title.
    OscTitle,
    /// The rest of any other.
    OscOther,
    /// A device control, privacy, application or start-of-string string.
    /// Nothing in one is looked at; it only has to be seen to its end.
    Str,
}

/// A place in a chunk of output where the stream may be cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafePoint {
    /// Offset just past the last byte after which the stream was safe.
    pub offset: usize,
    /// The modes in effect at that offset.
    pub modes: Arc<Modes>,
}

/// See the module documentation.
#[derive(Debug)]
pub struct Scanner {
    state: State,
    /// Continuation bytes the current UTF-8 character still needs.
    utf8_pending: u8,
    sync_open: bool,
    sync_bytes: usize,

    /// The private marker of the sequence being read (`?`, `>`, `<`, `=`),
    /// or 0.
    marker: u8,
    intermediate: u8,
    intermediates: u8,
    params: [u16; MAX_PARAMS],
    /// Parameters begun, the one being read included.
    param_count: usize,
    /// Set while a colon-separated sub-parameter is skipped.
    in_subparam: bool,

    osc_number: u16,
    title: Vec<u8>,

    modes: Modes,
    /// The last snapshot handed out; current unless `dirty`.
    shared: Arc<Modes>,
    dirty: bool,
    /// The snapshot belonging to the safe point of the feed in progress,
    /// taken at the last moment before a mode changes after it.
    held: Option<Arc<Modes>>,
    hold_wanted: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            utf8_pending: 0,
            sync_open: false,
            sync_bytes: 0,
            marker: 0,
            intermediate: 0,
            intermediates: 0,
            params: [0; MAX_PARAMS],
            param_count: 0,
            in_subparam: false,
            osc_number: 0,
            // Reserved once, so that reading a title never allocates.
            title: Vec::with_capacity(TITLE_MAX),
            modes: Modes::default(),
            shared: Arc::new(Modes::default()),
            dirty: false,
            held: None,
            hold_wanted: false,
        }
    }

    /// True when the stream may be cut right here: no sequence, no
    /// character and no synchronized update is open.
    pub fn is_safe(&self) -> bool {
        self.state == State::Ground && self.utf8_pending == 0 && !self.sync_open
    }

    /// The modes in effect after everything fed so far.
    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    /// The modes in effect after everything fed so far, shared. The same
    /// allocation is handed out until a mode changes.
    pub fn snapshot(&mut self) -> Arc<Modes> {
        if self.dirty {
            self.shared = Arc::new(self.modes.clone());
            self.dirty = false;
        }
        self.shared.clone()
    }

    /// [`Scanner::snapshot`] if the stream may be cut here.
    pub fn safe_snapshot(&mut self) -> Option<Arc<Modes>> {
        self.is_safe().then(|| self.snapshot())
    }

    /// Read `bytes`. Returns the last place in them where the stream may be
    /// cut, with the modes in effect there, or `None` if there is none.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<SafePoint> {
        let mut last_safe = None;
        self.held = None;
        self.hold_wanted = false;
        let mut at = 0;
        while at < bytes.len() {
            let before = at;
            if self.state == State::Ground && self.utf8_pending == 0 {
                // Most output is plain text, so it is skipped as a run and
                // not looked at byte by byte.
                while at < bytes.len() && bytes[at] < 0x80 && bytes[at] != ESC {
                    at += 1;
                }
                if at == before {
                    self.ground(bytes[at]);
                    at += 1;
                }
            } else {
                self.step(bytes[at]);
                at += 1;
            }
            if self.sync_open {
                self.sync_bytes += at - before;
                if self.sync_bytes > SYNC_MAX_BYTES {
                    self.sync_open = false;
                }
            }
            if self.is_safe() {
                last_safe = Some(at);
                self.held = None;
                self.hold_wanted = true;
            }
        }
        let offset = last_safe?;
        // Nothing changed after the safe point unless a snapshot was taken
        // for it then.
        let modes = match self.held.take() {
            Some(held) => held,
            None => self.snapshot(),
        };
        self.hold_wanted = false;
        Some(SafePoint { offset, modes })
    }

    /// Called before a mode changes. Keeps the modes of the feed's safe
    /// point while they are still the current ones.
    fn changing(&mut self) -> &mut Modes {
        if self.hold_wanted {
            self.held = Some(self.snapshot());
            self.hold_wanted = false;
        }
        self.dirty = true;
        &mut self.modes
    }

    /// A byte of 0x80 and above, or an escape, outside any sequence.
    fn ground(&mut self, byte: u8) {
        if self.utf8_pending > 0 {
            if (0x80..=0xbf).contains(&byte) {
                self.utf8_pending -= 1;
                return;
            }
            // Not a continuation: the character is abandoned and this byte
            // starts over.
            self.utf8_pending = 0;
        }
        match byte {
            ESC => self.enter_escape(),
            0xc2..=0xdf => self.utf8_pending = 1,
            0xe0..=0xef => self.utf8_pending = 2,
            0xf0..=0xf4 => self.utf8_pending = 3,
            // Plain text, a control character, or a byte that begins
            // nothing.
            _ => {}
        }
    }

    fn enter_escape(&mut self) {
        self.state = State::Escape;
        self.marker = 0;
        self.intermediate = 0;
        self.intermediates = 0;
    }

    /// A byte inside a sequence or a string.
    fn step(&mut self, byte: u8) {
        if self.state == State::Ground {
            return self.ground(byte);
        }
        // These three mean the same wherever they arrive. An escape also
        // ends a string: when a backslash follows it was the terminator,
        // and otherwise the string is abandoned for a new sequence, which
        // comes to the same thing here.
        match byte {
            CAN | SUB => {
                self.end_osc();
                self.state = State::Ground;
                return;
            }
            ESC => {
                self.end_osc();
                return self.enter_escape();
            }
            _ => {}
        }
        match self.state {
            State::Ground => {}
            State::Escape => match byte {
                0x20..=0x2f => {
                    self.collect(byte);
                    self.state = State::EscapeIntermediate;
                }
                b'[' => {
                    self.params = [0; MAX_PARAMS];
                    self.param_count = 0;
                    self.in_subparam = false;
                    self.state = State::Csi;
                }
                b']' => {
                    self.osc_number = 0;
                    self.state = State::OscNumber;
                }
                b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
                0x30..=0x7e => {
                    self.escape_final(byte);
                    self.state = State::Ground;
                }
                // Control characters act and leave the sequence open; the
                // rest is ignored.
                _ => {}
            },
            State::EscapeIntermediate => match byte {
                0x20..=0x2f => self.collect(byte),
                0x30..=0x7e => self.state = State::Ground,
                _ => {}
            },
            State::Csi => match byte {
                b'0'..=b'9' => self.digit(byte),
                b';' => {
                    self.param_count = self.param_count.max(1) + 1;
                    self.in_subparam = false;
                }
                b':' if self.param_count > 0 => self.in_subparam = true,
                b'<'..=b'?' if self.param_count == 0 && self.marker == 0 => self.marker = byte,
                b':' | b'<'..=b'?' => self.state = State::CsiIgnore,
                0x20..=0x2f => {
                    self.collect(byte);
                    self.state = State::CsiIntermediate;
                }
                0x40..=0x7e => {
                    self.csi_final(byte);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiIntermediate => match byte {
                0x20..=0x2f => self.collect(byte),
                0x30..=0x3f => self.state = State::CsiIgnore,
                0x40..=0x7e => {
                    self.csi_final(byte);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiIgnore => {
                if let 0x40..=0x7e = byte {
                    self.state = State::Ground;
                }
            }
            State::OscNumber => match byte {
                BEL => self.state = State::Ground,
                b'0'..=b'9' => {
                    self.osc_number = self
                        .osc_number
                        .saturating_mul(10)
                        .saturating_add(u16::from(byte - b'0'));
                }
                b';' if matches!(self.osc_number, 0 | 2) => {
                    self.title.clear();
                    self.state = State::OscTitle;
                }
                _ => self.state = State::OscOther,
            },
            State::OscTitle => match byte {
                BEL => {
                    self.end_osc();
                    self.state = State::Ground;
                }
                // Control characters are dropped, as an emulator drops
                // them, and so that a title can never end the string it is
                // sent back in.
                0x00..=0x1f | 0x7f => {}
                _ if self.title.len() < TITLE_MAX => self.title.push(byte),
                _ => {}
            },
            State::OscOther => {
                if byte == BEL {
                    self.state = State::Ground;
                }
            }
            State::Str => {}
        }
    }

    fn collect(&mut self, byte: u8) {
        self.intermediate = byte;
        self.intermediates = self.intermediates.saturating_add(1);
    }

    fn digit(&mut self, byte: u8) {
        self.param_count = self.param_count.max(1);
        if self.in_subparam {
            return;
        }
        if let Some(param) = self.params.get_mut(self.param_count - 1) {
            *param = param
                .saturating_mul(10)
                .saturating_add(u16::from(byte - b'0'));
        }
    }

    /// The parameter at `index`, or `default` when it is absent or zero.
    fn param(&self, index: usize, default: u16) -> u16 {
        match self.params.get(index) {
            Some(0) | None => default,
            Some(param) => *param,
        }
    }

    /// Finish a title string that was being read, if one was.
    fn end_osc(&mut self) {
        if self.state != State::OscTitle {
            return;
        }
        // A title cut at the limit may end inside a character.
        if self.title.len() == TITLE_MAX {
            if let Err(error) = std::str::from_utf8(&self.title) {
                if error.error_len().is_none() {
                    self.title.truncate(error.valid_up_to());
                }
            }
        }
        if self.modes.title.as_deref() != Some(self.title.as_slice()) {
            let title = Arc::from(self.title.as_slice());
            self.changing().title = Some(title);
        }
    }

    fn escape_final(&mut self, byte: u8) {
        match byte {
            b'c' => {
                // A full reset, the open synchronized update included.
                self.sync_open = false;
                if self.modes != Modes::default() {
                    *self.changing() = Modes::default();
                }
            }
            b'=' => self.set_switch(KEYPAD_APPLICATION, true),
            b'>' => self.set_switch(KEYPAD_APPLICATION, false),
            _ => {}
        }
    }

    fn set_switch(&mut self, bit: u8, set: bool) {
        let next = if set {
            self.modes.switches | bit
        } else {
            self.modes.switches & !bit
        };
        if next != self.modes.switches {
            self.changing().switches = next;
        }
    }

    fn csi_final(&mut self, byte: u8) {
        match (self.marker, self.intermediates, byte) {
            (b'?', 0, b'h' | b'l') => {
                for index in 0..self.param_count.clamp(1, MAX_PARAMS) {
                    self.private_mode(self.params[index], byte == b'h');
                }
            }
            (b'>', 0, b'u') => {
                let flags = self.param(0, 0);
                self.keyboard(|stack| stack.pushed(flags));
            }
            (b'<', 0, b'u') => {
                let count = self.param(0, 1);
                self.keyboard(|stack| stack.popped(count));
            }
            (b'=', 0, b'u') => {
                let (flags, how) = (self.param(0, 0), self.param(1, 1));
                self.keyboard(|stack| stack.set(flags, how));
            }
            (0, 1, b'q') if self.intermediate == b' ' => {
                let shape = self.param(0, 0);
                if shape <= 6 && shape as u8 != self.modes.cursor_shape {
                    self.changing().cursor_shape = shape as u8;
                }
            }
            (0, 1, b'p') if self.intermediate == b'!' => {
                // A soft reset. It leaves the screen, the mouse and the
                // paste mode alone and brings these back.
                self.set_switch(1 << 0, false);
                self.set_switch(1 << 1, true);
                self.set_switch(KEYPAD_APPLICATION, false);
            }
            _ => {}
        }
    }

    fn keyboard(&mut self, change: impl FnOnce(KittyStack) -> KittyStack) {
        let screen = usize::from(self.modes.alt_screen);
        let next = change(self.modes.kitty[screen]);
        if next != self.modes.kitty[screen] {
            self.changing().kitty[screen] = next;
        }
    }

    fn private_mode(&mut self, mode: u16, set: bool) {
        if mode == 2026 {
            self.sync_open = set;
            self.sync_bytes = 0;
        } else if mode == 1007 {
            if self.modes.alternate_scroll != Some(set) {
                self.changing().alternate_scroll = Some(set);
            }
        } else if let Some((_, bit, _)) = SWITCHES.iter().find(|(number, ..)| *number == mode) {
            self.set_switch(*bit, set);
        } else if let Some(index) = SCREEN_MODES.iter().position(|number| *number == mode) {
            let next = self.modes.screen.seen(index, set);
            if next != self.modes.screen || self.modes.alt_screen != set {
                let modes = self.changing();
                modes.screen = next;
                modes.alt_screen = set;
            }
        } else if let Some(index) = MOUSE_MODES.iter().position(|number| *number == mode) {
            let next = self.modes.mouse.seen(index, set);
            if next != self.modes.mouse {
                self.changing().mouse = next;
            }
        } else if let Some(index) = MOUSE_ENCODINGS.iter().position(|number| *number == mode) {
            let next = self.modes.mouse_encoding.seen(index, set);
            if next != self.modes.mouse_encoding {
                self.changing().mouse_encoding = next;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Scanner {
        let mut scanner = Scanner::new();
        scanner.feed(bytes);
        scanner
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).replace('\x1b', "^")
    }

    fn preamble_of(bytes: &[u8]) -> String {
        text(&scan(bytes).modes().preamble())
    }

    /// A stream with one of everything the scanner has a state for.
    pub(crate) fn mixed_fixture() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"plain \r\n");
        out.extend_from_slice("naïve — 日本語 🙂 ".as_bytes());
        out.extend_from_slice(b"\x1b[1;31mred\x1b[0m\x1b[?1049h\x1b[?2004h\x1b[?1000;1006h");
        out.extend_from_slice(b"\x1b]2;a title\x07\x1b]0;other \xe2\x9c\x93\x1b\\");
        out.extend_from_slice(b"\x1b]8;;http://example.com\x1b\\link\x1b]8;;\x1b\\");
        out.extend_from_slice(b"\x1bP+q544e\x1b\\\x1b_Gf=24;AAAA\x1b\\\x1b^pm\x1b\\\x1bXsos\x1b\\");
        out.extend_from_slice(b"\x1b[?2026h\x1b[2J\x1b[H\x1b[?25lframe\x1b[?25h\x1b[?2026l");
        out.extend_from_slice(b"\x1b[>5u\x1b[=1;2u\x1b[4 q\x1b=\x1b(B\x1b[38:2::1:2:3m");
        out.extend_from_slice(b"\x1b[?1h\x1b[?1004h\x1b[?1007l\x1b[?2031h\x1b[?1002h tail");
        out
    }

    #[test]
    fn plain_text_is_safe_after_every_byte() {
        let mut scanner = Scanner::new();
        assert!(scanner.is_safe());
        let safe = scanner.feed(b"hello\r\n\tworld\x07").unwrap();
        assert_eq!(safe.offset, 14);
        assert_eq!(*safe.modes, Modes::default());
        assert_eq!(scanner.feed(b""), None);
    }

    #[test]
    fn an_open_sequence_is_not_safe_until_its_final_byte() {
        for (sequence, name) in [
            (&b"\x1b[38;5;196m"[..], "a control sequence"),
            (b"\x1b(B", "an escape with an intermediate"),
            (b"\x1b]0;title\x07", "a command ended by a bell"),
            (b"\x1b]8;;http://x\x1b\\", "a command ended by a terminator"),
            (b"\x1bPq#0;2;0;0;0\x1b\\", "a device control string"),
            (b"\x1b_Ga=q\x1b\\", "an application string"),
            (b"\x1b^private\x1b\\", "a privacy message"),
            (b"\x1bXstring\x1b\\", "a start-of-string string"),
        ] {
            let mut scanner = Scanner::new();
            for (at, byte) in sequence.iter().enumerate() {
                assert!(
                    scanner.feed(&[*byte]).is_none() == (at + 1 < sequence.len()),
                    "{name}: wrong at byte {at}"
                );
            }
            assert!(scanner.is_safe(), "{name}");
        }
    }

    #[test]
    fn cancel_and_substitute_end_whatever_is_open() {
        for opener in [
            &b"\x1b"[..],
            b"\x1b[12;",
            b"\x1b]0;ti",
            b"\x1bPdata",
            b"\x1b_x",
        ] {
            for stop in [CAN, SUB] {
                let mut scanner = scan(opener);
                assert!(!scanner.is_safe());
                assert_eq!(scanner.feed(&[stop]).unwrap().offset, 1);
            }
        }
        // A cancelled title is still a title, as it is to an emulator.
        assert_eq!(scan(b"\x1b]2;cut\x18").modes().title(), Some(&b"cut"[..]));
    }

    #[test]
    fn an_escape_inside_a_sequence_starts_a_new_one() {
        let scanner = scan(b"\x1b[?25\x1b[?2004h");
        assert!(scanner.is_safe());
        assert_eq!(scanner.modes().switch(25), Some(true));
        assert_eq!(scanner.modes().switch(2004), Some(true));
        // A string that is abandoned for a sequence, not terminated.
        let scanner = scan(b"\x1bPdata\x1b[?1h");
        assert!(scanner.is_safe());
        assert_eq!(scanner.modes().switch(1), Some(true));
    }

    #[test]
    fn a_partial_character_is_not_safe() {
        let mut scanner = Scanner::new();
        let snowman = "☃".as_bytes();
        assert_eq!(scanner.feed(&snowman[..1]), None);
        assert_eq!(scanner.feed(&snowman[1..2]), None);
        assert_eq!(scanner.feed(&snowman[2..]).unwrap().offset, 1);

        // The safe point of a chunk that ends inside a character is before
        // that character.
        let mut scanner = Scanner::new();
        let mut chunk = b"ab".to_vec();
        chunk.extend_from_slice(&"🙂".as_bytes()[..3]);
        assert_eq!(scanner.feed(&chunk).unwrap().offset, 2);
        assert!(!scanner.is_safe());

        // A character that never gets its continuation is given up on by
        // the byte that is not one.
        let mut scanner = scan(&[0xe2, 0x98]);
        assert_eq!(scanner.feed(b"x").unwrap().offset, 1);
        // Bytes that begin nothing are safe at once.
        assert!(scan(&[0x80, 0xbf, 0xc0, 0xff]).is_safe());
    }

    #[test]
    fn an_open_synchronized_update_is_never_safe() {
        let mut scanner = Scanner::new();
        let safe = scanner
            .feed(b"before\x1b[?2026h\x1b[2Jframe \xe2\x9c\x93")
            .unwrap();
        assert_eq!(safe.offset, 6);
        assert_eq!(scanner.feed(b"more of the frame\r\n"), None);
        assert_eq!(scanner.feed(b"\x1b[?25l"), None);
        assert!(!scanner.is_safe());
        assert_eq!(scanner.feed(b"\x1b[?2026l").unwrap().offset, 8);

        // A reset ends one too.
        let mut scanner = scan(b"\x1b[?2026h");
        assert!(scanner.feed(b"\x1bc").is_some());
    }

    #[test]
    fn a_synchronized_update_that_is_never_closed_is_given_up_on() {
        let mut scanner = scan(b"\x1b[?2026h");
        let chunk = vec![b'x'; 64 * 1024];
        let mut fed = 0;
        while scanner.feed(&chunk).is_none() {
            fed += chunk.len();
            assert!(fed <= SYNC_MAX_BYTES + chunk.len(), "never given up on");
        }
        assert!(fed >= SYNC_MAX_BYTES - chunk.len());
    }

    #[test]
    fn the_modes_of_a_safe_point_are_the_ones_in_effect_there() {
        // The hide happens inside the update, after the last safe point of
        // the chunk, and must not show in that point's modes.
        let mut scanner = Scanner::new();
        let safe = scanner
            .feed(b"\x1b[?2004htext\x1b[?2026h\x1b[?25l")
            .unwrap();
        assert_eq!(safe.offset, 12);
        assert_eq!(safe.modes.switch(2004), Some(true));
        assert_eq!(safe.modes.switch(25), Some(true));
        assert_eq!(scanner.modes().switch(25), Some(false));

        // Closing the update makes a safe point that does have it.
        let safe = scanner.feed(b"\x1b[?2026l").unwrap();
        assert_eq!(safe.modes.switch(25), Some(false));
    }

    #[test]
    fn a_snapshot_is_shared_until_a_mode_changes() {
        let mut scanner = Scanner::new();
        let first = scanner.feed(b"some text").unwrap().modes;
        let second = scanner.feed(b"\x1b[31mmore\x1b[?25h").unwrap().modes;
        assert!(Arc::ptr_eq(&first, &second), "nothing changed");
        let third = scanner.feed(b"\x1b[?25l").unwrap().modes;
        assert!(!Arc::ptr_eq(&second, &third));
        assert!(Arc::ptr_eq(&third, &scanner.snapshot()));
    }

    #[test]
    fn every_plain_switch_is_set_and_reset() {
        for (mode, _, at_start) in SWITCHES {
            let mut scanner = Scanner::new();
            assert_eq!(scanner.modes().switch(mode), Some(at_start), "{mode}");
            scanner.feed(format!("\x1b[?{mode}h").as_bytes());
            assert_eq!(scanner.modes().switch(mode), Some(true), "{mode}");
            assert!(
                preamble_of(format!("\x1b[?{mode}h").as_bytes()).contains(&format!("^[?{mode}h"))
            );
            scanner.feed(format!("\x1b[?{mode}l").as_bytes());
            assert_eq!(scanner.modes().switch(mode), Some(false), "{mode}");
            assert!(text(&scanner.modes().preamble()).contains(&format!("^[?{mode}l")));
        }
        assert_eq!(Modes::default().switch(6), None);
    }

    #[test]
    fn alternate_scroll_is_restored_only_once_the_program_touched_it() {
        assert!(!preamble_of(b"text").contains("1007"));
        assert!(preamble_of(b"\x1b[?1007h").contains("^[?1007h"));
        assert!(preamble_of(b"\x1b[?1007h\x1b[?1007l").contains("^[?1007l"));
    }

    #[test]
    fn several_modes_in_one_sequence_are_all_seen() {
        let scanner = scan(b"\x1b[?1;25;2004;1049h");
        let modes = scanner.modes();
        assert_eq!(modes.switch(1), Some(true));
        assert_eq!(modes.switch(2004), Some(true));
        assert!(modes.alt_screen());
        let scanner = scan(b"\x1b[?1;2004h\x1b[?1;2004l");
        assert_eq!(scanner.modes().switch(1), Some(false));
        assert_eq!(scanner.modes().switch(2004), Some(false));
    }

    #[test]
    fn the_alternate_screen_is_tracked_under_all_three_spellings() {
        for mode in SCREEN_MODES {
            let mut scanner = scan(format!("\x1b[?{mode}h").as_bytes());
            assert!(scanner.modes().alt_screen(), "{mode}");
            assert!(text(&scanner.modes().preamble()).ends_with(&format!("^[?{mode}h^[?1l^[?7h^[?25h^[?1004l^[?2004l^[?2031l^[?1000l^[?1002l^[?1003l^[?1005l^[?1006l^[?1015l^[?1016l^>^[0 q")), "{mode}");
            scanner.feed(format!("\x1b[?{mode}l").as_bytes());
            assert!(!scanner.modes().alt_screen(), "{mode}");
        }
        // Entered under one spelling and left under another.
        assert!(!scan(b"\x1b[?1049h\x1b[?47l").modes().alt_screen());
    }

    #[test]
    fn related_modes_are_replayed_in_the_order_they_were_last_seen() {
        // An emulator in which setting one mouse mode clears the others
        // ends differently for these two streams; the order is all that
        // tells them apart.
        let preamble = preamble_of(b"\x1b[?1003h\x1b[?1000h");
        assert!(
            preamble.contains("^[?1000l^[?1002l^[?1003l^[?1003h^[?1000h"),
            "{preamble}"
        );
        let preamble = preamble_of(b"\x1b[?1000h\x1b[?1003h");
        assert!(preamble.contains("^[?1003l^[?1000h^[?1003h"), "{preamble}");
        // Seen again, a sequence moves to the end and is not repeated.
        let preamble = preamble_of(b"\x1b[?1000h\x1b[?1003h\x1b[?1000h\x1b[?1003l\x1b[?1000h");
        assert!(
            preamble.contains("^[?1003l^[?1003h^[?1003l^[?1000h^[?1005l"),
            "{preamble}"
        );
        let preamble = preamble_of(b"\x1b[?1006h\x1b[?1015h\x1b[?1016h\x1b[?1005h\x1b[?1015l");
        assert!(
            preamble.contains("^[?1016l^[?1006h^[?1015h^[?1016h^[?1005h^[?1015l^>"),
            "{preamble}"
        );
    }

    #[test]
    fn the_keypad_and_the_cursor_shape_are_tracked() {
        let scanner = scan(b"\x1b=\x1b[5 q");
        assert!(scanner.modes().keypad_application());
        assert_eq!(scanner.modes().cursor_shape(), 5);
        assert!(text(&scanner.modes().preamble()).ends_with("^=^[5 q"));
        let scanner = scan(b"\x1b=\x1b[5 q\x1b>\x1b[ q");
        assert!(!scanner.modes().keypad_application());
        assert_eq!(scanner.modes().cursor_shape(), 0);
        // Not a shape, and not a cursor-shape sequence at all.
        assert_eq!(scan(b"\x1b[9 q").modes().cursor_shape(), 0);
        assert_eq!(scan(b"\x1b[5q\x1b[?5 q\x1b[5!q").modes().cursor_shape(), 0);
    }

    #[test]
    fn each_screen_has_its_own_keyboard_stack() {
        let scanner = scan(b"\x1b[>1u\x1b[>3u\x1b[?1049h\x1b[>5u\x1b[=8;2u");
        assert_eq!(scanner.modes().keyboard_stack(false), [1, 3]);
        assert_eq!(scanner.modes().keyboard_stack(true), [13]);
        let preamble = text(&scanner.modes().preamble());
        assert!(
            preamble.contains("^[?1049h^[<99u^[>13u^[?1049l^[<99u^[>1u^[>3u^[?1049h"),
            "{preamble}"
        );

        let mut scanner = scanner;
        scanner.feed(b"\x1b[?1049l\x1b[<u");
        assert_eq!(scanner.modes().keyboard_stack(false), [1]);
        assert_eq!(scanner.modes().keyboard_stack(true), [13]);
        scanner.feed(b"\x1b[<5u");
        assert!(scanner.modes().keyboard_stack(false).is_empty());
    }

    #[test]
    fn keyboard_flags_are_set_replaced_united_and_removed() {
        assert_eq!(scan(b"\x1b[=5u").modes().keyboard_stack(false), [5]);
        assert_eq!(scan(b"\x1b[>1u\x1b[=6u").modes().keyboard_stack(false), [6]);
        assert_eq!(
            scan(b"\x1b[>1u\x1b[=6;2u").modes().keyboard_stack(false),
            [7]
        );
        assert_eq!(
            scan(b"\x1b[>7u\x1b[=5;3u").modes().keyboard_stack(false),
            [2]
        );
        assert_eq!(scan(b"\x1b[>u").modes().keyboard_stack(false), [0]);
        // Only the five flags the protocol defines.
        assert_eq!(scan(b"\x1b[>255u").modes().keyboard_stack(false), [31]);
        // A query changes nothing.
        assert!(scan(b"\x1b[?u").modes().keyboard_stack(false).is_empty());
    }

    #[test]
    fn a_keyboard_stack_that_overflows_drops_its_oldest_entry() {
        let mut scanner = Scanner::new();
        for flags in 1..=10 {
            scanner.feed(format!("\x1b[>{flags}u").as_bytes());
        }
        assert_eq!(
            scanner.modes().keyboard_stack(false),
            [3, 4, 5, 6, 7, 8, 9, 10]
        );
    }

    #[test]
    fn the_title_is_kept_and_bounded() {
        assert_eq!(scan(b"text").modes().title(), None);
        assert_eq!(scan(b"\x1b]0;one\x07").modes().title(), Some(&b"one"[..]));
        assert_eq!(scan(b"\x1b]2;a;b\x1b\\").modes().title(), Some(&b"a;b"[..]));
        assert_eq!(scan(b"\x1b]2;\x07").modes().title(), Some(&b""[..]));
        // Other commands, and an icon name, are not titles.
        assert_eq!(
            scan(b"\x1b]1;icon\x07\x1b]7;file:///\x07").modes().title(),
            None
        );
        assert_eq!(scan(b"\x1b]22;pointer\x07\x1b]2\x07").modes().title(), None);
        assert!(preamble_of(b"\x1b]2;shown\x07").ends_with("^]2;shown^\\"));

        // Control characters cannot ride along into the preamble.
        assert_eq!(
            scan(b"\x1b]2;a\x01\nb\x07").modes().title(),
            Some(&b"ab"[..])
        );

        let mut long = b"\x1b]2;".to_vec();
        long.extend_from_slice(&vec![b'x'; TITLE_MAX - 1]);
        long.extend_from_slice("☃☃☃".as_bytes());
        long.push(BEL);
        let scanner = scan(&long);
        let title = scanner.modes().title().unwrap();
        // Cut at the limit, and then back to the end of a character.
        assert_eq!(title.len(), TITLE_MAX - 1);
        assert!(std::str::from_utf8(title).is_ok());
        assert!(scanner.is_safe());
    }

    #[test]
    fn a_full_reset_brings_everything_back_and_a_soft_one_a_little() {
        let dirty = b"\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1h\x1b[?25l\x1b=\x1b[>1u\x1b[3 q\x1b]2;t\x07\x1b[?1007h";
        let mut scanner = scan(dirty);
        assert_ne!(*scanner.modes(), Modes::default());
        scanner.feed(b"\x1bc");
        assert_eq!(*scanner.modes(), Modes::default());

        let mut scanner = scan(dirty);
        scanner.feed(b"\x1b[!p");
        let modes = scanner.modes();
        assert_eq!(modes.switch(1), Some(false));
        assert_eq!(modes.switch(25), Some(true));
        assert!(!modes.keypad_application());
        assert!(modes.alt_screen());
        assert_eq!(modes.switch(2004), Some(true));
    }

    #[test]
    fn malformed_sequences_do_nothing_and_end_where_they_should() {
        // A marker in the wrong place, a colon first, a parameter after an
        // intermediate: each runs to its final byte and is dropped.
        for sequence in [&b"\x1b[25?h"[..], b"\x1b[:25h", b"\x1b[ 5h", b"\x1b[?25?l"] {
            let scanner = scan(sequence);
            assert!(scanner.is_safe());
            assert_eq!(*scanner.modes(), Modes::default());
        }
        // Without the marker it is another mode altogether.
        assert_eq!(scan(b"\x1b[25l\x1b[2004h").modes(), &Modes::default());
        // More parameters than are kept do not spill into anything.
        let mut many = b"\x1b[?".to_vec();
        for _ in 0..40 {
            many.extend_from_slice(b"2004;");
        }
        many.extend_from_slice(b"1h");
        let scanner = scan(&many);
        assert_eq!(scanner.modes().switch(2004), Some(true));
        assert_eq!(scanner.modes().switch(1), Some(false));
        // A control character inside a sequence acts and leaves it open.
        assert_eq!(scan(b"\x1b[?20\r04h").modes().switch(2004), Some(true));
    }

    #[test]
    fn feeding_byte_by_byte_ends_like_feeding_whole() {
        let fixture = mixed_fixture();
        let mut whole = Scanner::new();
        let whole_safe = whole.feed(&fixture).unwrap();
        assert_eq!(whole_safe.offset, fixture.len());

        // The safe offsets of the stream, found one byte at a time.
        let mut single = Scanner::new();
        let mut safe_after = vec![false; fixture.len() + 1];
        safe_after[0] = true;
        let mut modes_after = vec![Arc::new(Modes::default())];
        for (at, byte) in fixture.iter().enumerate() {
            if let Some(safe) = single.feed(&[*byte]) {
                assert_eq!(safe.offset, 1);
                safe_after[at + 1] = true;
                assert_eq!(*safe.modes, *single.modes());
            }
            modes_after.push(single.snapshot());
        }
        assert_eq!(single.modes(), whole.modes());
        assert_eq!(*whole_safe.modes, *single.modes());
        assert!(safe_after.iter().filter(|safe| **safe).count() > 20);
        assert!(safe_after.iter().filter(|safe| !**safe).count() > 100);

        // Torn at every offset: both halves agree with the single bytes
        // about where the stream is safe and what is in effect there.
        for cut in 0..=fixture.len() {
            let mut torn = Scanner::new();
            let head = torn.feed(&fixture[..cut]);
            let expected = (1..=cut).rev().find(|at| safe_after[*at]);
            assert_eq!(head.as_ref().map(|safe| safe.offset), expected, "cut {cut}");
            if let Some(safe) = head {
                assert_eq!(*safe.modes, *modes_after[safe.offset], "cut {cut}");
            }
            assert_eq!(torn.is_safe(), safe_after[cut], "cut {cut}");
            let tail = torn.feed(&fixture[cut..]);
            let expected = (cut + 1..=fixture.len()).rev().find(|at| safe_after[*at]);
            assert_eq!(
                tail.as_ref().map(|safe| safe.offset + cut),
                expected,
                "cut {cut}"
            );
            if let Some(safe) = tail {
                assert_eq!(*safe.modes, *modes_after[safe.offset + cut], "cut {cut}");
            }
            assert_eq!(torn.modes(), whole.modes(), "cut {cut}");
        }
    }

    #[test]
    fn a_preamble_fed_back_gives_the_modes_it_was_made_from() {
        let fixture = mixed_fixture();
        let mut scanner = Scanner::new();
        for cut in 0..=fixture.len() {
            scanner.feed(&fixture[cut.saturating_sub(1)..cut]);
            let modes = scanner.modes().clone();
            // From nothing, and from a scanner that is somewhere else
            // entirely: in a string, in an update, in other modes.
            for before in [
                &b""[..],
                b"\x1b[?1049h\x1b[?1003h\x1b[?1015h\x1b[>9u\x1b[?2026h\x1b]2;old",
            ] {
                let mut replayed = scan(before);
                assert!(replayed.feed(&modes.preamble()).is_some(), "cut {cut}");
                let mut got = replayed.modes().clone();
                // A title cannot be taken away again, and alternate scroll
                // is left as it is until the program touches it.
                if modes.title.is_none() {
                    got.title = None;
                }
                if modes.alternate_scroll.is_none() {
                    got.alternate_scroll = None;
                }
                assert_eq!(got.alt_screen, modes.alt_screen, "cut {cut}");
                assert_eq!(got.switches, modes.switches, "cut {cut}");
                assert_eq!(got.kitty, modes.kitty, "cut {cut}");
                assert_eq!(got.cursor_shape, modes.cursor_shape, "cut {cut}");
                assert_eq!(got.title, modes.title, "cut {cut}");
                assert_eq!(got.alternate_scroll, modes.alternate_scroll, "cut {cut}");
                // The preamble's own resets come first and the program's
                // sequences after them, in their order.
                for (got, want) in [
                    (got.screen, modes.screen),
                    (got.mouse, modes.mouse),
                    (got.mouse_encoding, modes.mouse_encoding),
                ] {
                    let (got, want): (Vec<_>, Vec<_>) =
                        (got.iter().collect(), want.iter().collect());
                    assert!(got.ends_with(&want), "cut {cut}: {got:?} {want:?}");
                }
            }
        }
    }

    /// A small deterministic generator, so that a failing stream can be
    /// made again from its seed.
    pub(crate) struct Rng(pub(crate) u64);

    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        pub(crate) fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }

        fn pick<T: Copy>(&mut self, from: &[T]) -> T {
            from[self.below(from.len())]
        }
    }

    /// What a synthetic stream may contain. The parts that emulators
    /// disagree about can be left out, for a test that compares against
    /// one emulator.
    #[derive(Clone, Copy)]
    pub(crate) struct Mix {
        /// `47` and `1047`, which not every emulator takes for the
        /// alternate screen.
        pub(crate) old_screen_modes: bool,
        pub(crate) keyboard: bool,
        pub(crate) full_reset: bool,
        /// Mode 1007, whose state after a reset differs between emulators.
        pub(crate) alternate_scroll: bool,
    }

    impl Mix {
        pub(crate) const EVERYTHING: Mix = Mix {
            old_screen_modes: true,
            keyboard: true,
            full_reset: true,
            alternate_scroll: true,
        };
    }

    /// A made-up session, and every offset at which it may be cut: the
    /// boundaries between its pieces that lie outside a synchronized
    /// update. The generator knows them by construction, which makes them
    /// a check on the scanner that does not go through the scanner.
    pub(crate) struct Synthetic {
        pub(crate) bytes: Vec<u8>,
        pub(crate) safe: Vec<usize>,
    }

    struct Generator {
        rng: Rng,
        mix: Mix,
        out: Vec<u8>,
        safe: Vec<usize>,
        in_update: bool,
        alt: bool,
        /// Depth of each screen's keyboard stack, kept within what every
        /// implementation holds.
        depth: [usize; 2],
    }

    impl Generator {
        fn put(&mut self, bytes: &[u8]) {
            self.out.extend_from_slice(bytes);
            if !self.in_update {
                self.safe.push(self.out.len());
            }
        }

        fn mode(&mut self) -> u16 {
            let mut modes = vec![
                1, 25, 1004, 2004, 2031, 1049, 1049, 1000, 1002, 1003, 1005, 1006, 1015, 1016,
            ];
            if self.mix.old_screen_modes {
                modes.extend([47, 1047]);
            }
            if self.mix.alternate_scroll {
                modes.push(1007);
            }
            self.rng.pick(&modes)
        }

        fn note_screen(&mut self, mode: u16, set: bool) {
            if SCREEN_MODES.contains(&mode) {
                self.alt = set;
            }
        }

        fn piece(&mut self) {
            match self.rng.below(100) {
                0..=39 => {
                    for _ in 0..=self.rng.below(6) {
                        let text = self
                            .rng
                            .pick(&["a", "Z", " ", "\r", "\n", "é", "日", "🙂", "\t"]);
                        self.put(text.as_bytes());
                    }
                }
                40..=49 => {
                    let (a, b) = (self.rng.below(40), self.rng.below(120));
                    let sequence = match self.rng.below(5) {
                        0 => format!("\x1b[{a}m"),
                        1 => format!("\x1b[38;5;{b}m"),
                        2 => format!("\x1b[{};{}H", a + 1, b + 1),
                        3 => "\x1b[2J".to_string(),
                        _ => "\x1b[K".to_string(),
                    };
                    self.put(sequence.as_bytes());
                }
                50..=74 => {
                    let set = self.rng.below(2) == 0;
                    let letter = if set { 'h' } else { 'l' };
                    let first = self.mode();
                    self.note_screen(first, set);
                    if self.rng.below(5) == 0 {
                        let second = self.mode();
                        self.note_screen(second, set);
                        self.put(format!("\x1b[?{first};{second}{letter}").as_bytes());
                    } else {
                        self.put(format!("\x1b[?{first}{letter}").as_bytes());
                    }
                }
                75..=78 => {
                    let keypad = self.rng.pick(&[&b"\x1b="[..], b"\x1b>"]);
                    self.put(keypad);
                }
                79..=82 => {
                    let shape = self.rng.below(7);
                    self.put(format!("\x1b[{shape} q").as_bytes());
                }
                83..=88 if self.mix.keyboard => {
                    let screen = usize::from(self.alt);
                    let push = self.depth[screen] == 0
                        || (self.depth[screen] < KITTY_DEPTH && self.rng.below(2) == 0);
                    if push {
                        self.depth[screen] += 1;
                        let flags = self.rng.below(32);
                        self.put(format!("\x1b[>{flags}u").as_bytes());
                    } else {
                        self.depth[screen] -= 1;
                        self.put(b"\x1b[<u");
                    }
                }
                89..=91 => {
                    let title = self.rng.pick(&["shell", "vim — notes", "", "a;b"]);
                    let end = self.rng.pick(&["\x07", "\x1b\\"]);
                    let number = self.rng.pick(&[0, 2]);
                    self.put(format!("\x1b]{number};{title}{end}").as_bytes());
                }
                92..=94 => {
                    let string = self.rng.pick(&[
                        &b"\x1bP+q544e\x1b\\"[..],
                        b"\x1b_Gf=24,s=1,v=1;AAAA\x1b\\",
                        b"\x1b]8;;http://example.com\x1b\\",
                        b"\x1b]133;A\x07",
                    ]);
                    self.put(string);
                }
                95..=98 if !self.in_update => {
                    self.in_update = true;
                    self.put(b"\x1b[?2026h");
                    for _ in 0..=self.rng.below(8) {
                        self.piece();
                    }
                    self.in_update = false;
                    self.put(b"\x1b[?2026l");
                }
                99 if self.mix.full_reset && !self.in_update => {
                    self.alt = false;
                    self.depth = [0; 2];
                    self.put(b"\x1bc");
                }
                _ => self.put(b"."),
            }
        }
    }

    pub(crate) fn synthetic(seed: u64, pieces: usize, mix: Mix) -> Synthetic {
        let mut generator = Generator {
            // Any odd constant: the generator must not start at zero.
            rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1),
            mix,
            out: Vec::new(),
            safe: vec![0],
            in_update: false,
            alt: false,
            depth: [0; 2],
        };
        for _ in 0..pieces {
            generator.piece();
        }
        Synthetic {
            bytes: generator.out,
            safe: generator.safe,
        }
    }

    #[test]
    fn the_scanner_finds_exactly_the_boundaries_a_stream_was_built_with() {
        for seed in 1..=40 {
            let stream = synthetic(seed, 300, Mix::EVERYTHING);
            let mut scanner = Scanner::new();
            let mut found = vec![0];
            for (at, byte) in stream.bytes.iter().enumerate() {
                if scanner.feed(&[*byte]).is_some() {
                    found.push(at + 1);
                }
            }
            assert_eq!(found, stream.safe, "seed {seed}");
        }
    }
}
