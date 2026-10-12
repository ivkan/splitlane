//! The terminal a session's program talks to.
//!
//! A program does not only print. It asks its terminal where the cursor is,
//! what it supports and what its colours are, and waits for the answer;
//! some ask on every resize. A session has to get those answers whether or
//! not a window is attached, so the host gives them itself: each session
//! has an [`Engine`], an emulator without a screen anybody looks at, which
//! reads the session's output in the order the log records it and writes
//! back what a terminal would.
//!
//! The answers have to be the ones the window's own emulator gives, or a
//! program would see one terminal while it starts and another once a window
//! shows it: a keyboard protocol it was told is supported and then is not.
//! So the engine is, on each platform, the emulator the window uses there,
//! configured the same way. On macOS that is `alacritty_terminal`. Where
//! the window uses another emulator the host has no engine yet and answers
//! nothing; [`for_this_platform`] returns `None` there, and the rest of the
//! host does not care which it got.

use std::time::Instant;

use crate::protocol::Facts;

/// An emulator that follows one session and answers its program.
pub trait Engine: Send {
    /// Read output of the session. Whatever the terminal answers to it is
    /// appended to `replies`, in order.
    fn advance(&mut self, bytes: &[u8], replies: &mut Vec<u8>);

    /// The terminal was resized. `cell` is the size of a character cell in
    /// pixels if the resize came with one; `None` leaves it as it was.
    fn resize(&mut self, cols: u16, rows: u16, cell: Option<(u16, u16)>);

    /// Replace what is known about the window. See [`Facts`].
    fn set_facts(&mut self, facts: &Facts);

    /// When output that is being held back for a synchronized update has
    /// to be let through even though the program has not ended the update.
    /// `None` while nothing is held back.
    fn sync_deadline(&self) -> Option<Instant>;

    /// Let held-back output through; see [`Engine::sync_deadline`].
    fn end_sync(&mut self, replies: &mut Vec<u8>);
}

/// The engine of this platform for a terminal of the given size, or `None`
/// where the host has none.
pub fn for_this_platform(cols: u16, rows: u16) -> Option<Box<dyn Engine>> {
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(alacritty::AlacrittyEngine::new(cols, rows)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (cols, rows);
        None
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod alacritty {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use alacritty_terminal::event::{Event, EventListener, WindowSize};
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{NamedColor, Processor, Rgb, StdSyncHandler};

    use super::Engine;
    use crate::lock;
    use crate::protocol::Facts;

    /// The smallest grid the emulator is built for.
    const MIN_COLS: u16 = 2;
    const MIN_ROWS: u16 = 1;

    struct Size {
        cols: u16,
        rows: u16,
    }

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.rows as usize
        }

        fn screen_lines(&self) -> usize {
            self.rows as usize
        }

        fn columns(&self) -> usize {
            self.cols as usize
        }
    }

    /// What the emulator's events are turned into, and what that takes.
    #[derive(Default)]
    struct Outbox {
        replies: Vec<u8>,
        facts: Facts,
        cell: Option<(u16, u16)>,
        cols: u16,
        rows: u16,
    }

    impl Outbox {
        /// The colour a query for `index` is answered with. The emulator
        /// numbers the palette 0 to 255 and the foreground, background and
        /// cursor after it.
        fn colour(&self, index: usize) -> Option<Rgb> {
            let colour = if index == NamedColor::Foreground as usize {
                self.facts.foreground
            } else if index == NamedColor::Background as usize {
                self.facts.background
            } else if index == NamedColor::Cursor as usize {
                self.facts.cursor
            } else {
                u8::try_from(index)
                    .ok()
                    .and_then(|index| self.facts.palette.get(&index).copied())
            }?;
            Some(Rgb {
                r: colour.0,
                g: colour.1,
                b: colour.2,
            })
        }
    }

    /// Receives the emulator's events while it reads output. Only the ones
    /// that are answers to the program become anything.
    #[derive(Clone)]
    struct Listener(Arc<Mutex<Outbox>>);

    impl EventListener for Listener {
        fn send_event(&self, event: Event) {
            let mut outbox = lock(&self.0);
            match event {
                Event::PtyWrite(text) => outbox.replies.extend_from_slice(text.as_bytes()),
                Event::ColorRequest(index, format) => {
                    // No colour known is no answer, not a made-up one: the
                    // program then falls back as it does in a terminal that
                    // cannot be asked.
                    if let Some(colour) = outbox.colour(index) {
                        let answer = format(colour);
                        outbox.replies.extend_from_slice(answer.as_bytes());
                    }
                }
                Event::TextAreaSizeRequest(format) => {
                    if let Some((cell_width, cell_height)) = outbox.cell {
                        let answer = format(WindowSize {
                            num_lines: outbox.rows,
                            num_cols: outbox.cols,
                            cell_width,
                            cell_height,
                        });
                        outbox.replies.extend_from_slice(answer.as_bytes());
                    }
                }
                // The title, the bell and a copy to the clipboard are for
                // a window. A paste from the clipboard is a question, but
                // one there is nobody here to ask.
                _ => {}
            }
        }
    }

    pub(crate) struct AlacrittyEngine {
        term: Term<Listener>,
        processor: Processor<StdSyncHandler>,
        outbox: Arc<Mutex<Outbox>>,
    }

    impl AlacrittyEngine {
        pub(crate) fn new(cols: u16, rows: u16) -> Self {
            // The settings a pane's terminal is built with in the app,
            // which leaves everything at the emulator's defaults except
            // the two below: the keyboard protocol in particular stays
            // off, so a query for it gets no answer here either. The
            // cursor's default shape, which the app takes from the user's
            // configuration, changes no answer and is left alone.
            Self::with_config(
                Config {
                    // The screen is kept for where the cursor is. Nobody
                    // scrolls back in it.
                    scrolling_history: 0,
                    ..Config::default()
                },
                cols,
                rows,
            )
        }

        pub(crate) fn with_config(config: Config, cols: u16, rows: u16) -> Self {
            let (cols, rows) = (cols.max(MIN_COLS), rows.max(MIN_ROWS));
            let outbox = Arc::new(Mutex::new(Outbox {
                cols,
                rows,
                ..Outbox::default()
            }));
            Self {
                term: Term::new(config, &Size { cols, rows }, Listener(outbox.clone())),
                processor: Processor::new(),
                outbox,
            }
        }

        #[cfg(test)]
        pub(crate) fn term(&self) -> &Term<impl EventListener> {
            &self.term
        }

        fn collect(&mut self, replies: &mut Vec<u8>) {
            replies.append(&mut lock(&self.outbox).replies);
        }
    }

    impl Engine for AlacrittyEngine {
        fn advance(&mut self, bytes: &[u8], replies: &mut Vec<u8>) {
            self.processor.advance(&mut self.term, bytes);
            self.collect(replies);
        }

        fn resize(&mut self, cols: u16, rows: u16, cell: Option<(u16, u16)>) {
            let (cols, rows) = (cols.max(MIN_COLS), rows.max(MIN_ROWS));
            self.term.resize(Size { cols, rows });
            let mut outbox = lock(&self.outbox);
            outbox.cols = cols;
            outbox.rows = rows;
            if cell.is_some() {
                outbox.cell = cell;
            }
        }

        fn set_facts(&mut self, facts: &Facts) {
            let mut outbox = lock(&self.outbox);
            if let (Some(width), Some(height)) = (facts.cell_width, facts.cell_height) {
                outbox.cell = Some((width, height));
            }
            outbox.facts = facts.clone();
        }

        fn sync_deadline(&self) -> Option<Instant> {
            self.processor.sync_timeout().sync_timeout()
        }

        fn end_sync(&mut self, replies: &mut Vec<u8>) {
            self.processor.stop_sync(&mut self.term);
            self.collect(replies);
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use alacritty_terminal::term::Config;

    use super::alacritty::AlacrittyEngine;
    use super::*;
    use crate::log::{Item, SessionLog};
    use crate::protocol::Rgb;
    use crate::scan::tests::{synthetic, Mix, Rng};
    use crate::scan::{Modes, Scanner};

    fn ask(engine: &mut dyn Engine, query: &[u8]) -> String {
        let mut replies = Vec::new();
        engine.advance(query, &mut replies);
        String::from_utf8(replies).unwrap().replace('\x1b', "^")
    }

    /// The answers a pane of the app gives on this platform, written out.
    /// The app has a test of its own,
    /// `the_session_host_answers_queries_like_a_pane_does`, that asks a
    /// terminal built by the app's own code the same questions and compares;
    /// this one says what the answers are.
    #[test]
    fn queries_get_the_answers_the_window_gives() {
        let mut engine = AlacrittyEngine::new(80, 24);
        assert_eq!(ask(&mut engine, b"\x1b[6n"), "^[1;1R");
        assert_eq!(ask(&mut engine, b"abc\r\nde\x1b[6n"), "^[2;3R");
        assert_eq!(ask(&mut engine, b"\x1b[5n"), "^[0n");
        assert_eq!(ask(&mut engine, b"\x1b[c"), "^[?6c");
        assert_eq!(ask(&mut engine, b"\x1b[>c"), "^[>0;2600;1c");
        assert_eq!(ask(&mut engine, b"\x1b[18t"), "^[8;24;80t");
        assert_eq!(ask(&mut engine, b"\x1b[?2004$p"), "^[?2004;2$y");
        assert_eq!(ask(&mut engine, b"\x1b[?2004h\x1b[?2004$p"), "^[?2004;1$y");
        assert_eq!(ask(&mut engine, b"\x1b[?2026$p"), "^[?2026;2$y");
        assert_eq!(ask(&mut engine, b"\x1b[?9999$p"), "^[?9999;0$y");
        assert_eq!(ask(&mut engine, b"\x1b[4$p"), "^[4;2$y");
        // Several at once come back in the order they were asked.
        assert_eq!(ask(&mut engine, b"\x1b[5n\x1b[c\x1b[5n"), "^[0n^[?6c^[0n");

        // What a pane does not answer is not answered here: the keyboard
        // protocol, which its terminal is built without, the terminal's
        // name and version, and a paste from the clipboard.
        assert_eq!(ask(&mut engine, b"\x1b[?u"), "");
        assert_eq!(ask(&mut engine, b"\x1b[>1u\x1b[?u"), "");
        assert_eq!(ask(&mut engine, b"\x1b[>q"), "");
        assert_eq!(ask(&mut engine, b"\x1b]52;c;?\x07"), "");
    }

    #[test]
    fn the_cursor_is_where_the_output_and_the_resizes_left_it() {
        let mut engine = AlacrittyEngine::new(80, 24);
        assert_eq!(ask(&mut engine, b"\x1b[20;70H\x1b[6n"), "^[20;70R");
        engine.resize(40, 10, None);
        // The line the cursor was on is wrapped at the new width, and the
        // cursor goes with its place in the text.
        assert_eq!(ask(&mut engine, b"\x1b[6n\x1b[18t"), "^[10;30R^[8;10;40t");
        // The alternate screen has a cursor of its own.
        assert_eq!(ask(&mut engine, b"\x1b[?1049h\x1b[H\x1b[6n"), "^[1;1R");
        assert_eq!(ask(&mut engine, b"\x1b[?1049l\x1b[6n"), "^[10;30R");
    }

    #[test]
    fn what_only_a_window_knows_is_answered_once_it_was_told() {
        let mut engine = AlacrittyEngine::new(80, 24);
        let colours = b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07\x1b]4;1;?\x07\x1b[14t";
        // A terminal that cannot be asked, and a program that goes on to
        // the question it uses to tell: only that one is answered.
        assert_eq!(ask(&mut engine, colours), "");
        assert_eq!(ask(&mut engine, b"\x1b]11;?\x07\x1b[c"), "^[?6c");

        engine.set_facts(&Facts {
            foreground: Some(Rgb(0xd0, 0xd1, 0xd2)),
            background: Some(Rgb(0x12, 0x34, 0x56)),
            cursor: None,
            palette: BTreeMap::from([(1, Rgb(0xff, 0x00, 0x7f))]),
            cell_width: Some(9),
            cell_height: Some(18),
        });
        // Each answer ends the way its question did.
        assert_eq!(
            ask(&mut engine, colours),
            "^]10;rgb:d0d0/d1d1/d2d2\x07^]11;rgb:1212/3434/5656^\\^]4;1;rgb:ffff/0000/7f7f\x07^[4;432;720t"
        );
        assert_eq!(ask(&mut engine, b"\x1b]4;2;?\x07"), "");

        // A resize that came with pixels changes the cell; one without
        // keeps it.
        engine.resize(100, 30, Some((10, 20)));
        assert_eq!(ask(&mut engine, b"\x1b[14t"), "^[4;600;1000t");
        engine.resize(50, 10, None);
        assert_eq!(ask(&mut engine, b"\x1b[14t"), "^[4;200;500t");

        // New facts replace the old ones whole.
        engine.set_facts(&Facts::default());
        assert_eq!(ask(&mut engine, b"\x1b]11;?\x07\x1b]4;1;?\x07"), "");
    }

    #[test]
    fn a_query_inside_a_synchronized_update_waits_for_its_end() {
        let mut engine = AlacrittyEngine::new(80, 24);
        assert_eq!(engine.sync_deadline(), None);
        assert_eq!(ask(&mut engine, b"\x1b[?2026h\x1b[6n"), "");
        assert!(engine.sync_deadline().is_some());
        assert_eq!(ask(&mut engine, b"\x1b[?2026l"), "^[1;1R");
        assert_eq!(engine.sync_deadline(), None);

        // An update the program never ends is ended for it.
        assert_eq!(ask(&mut engine, b"\x1b[?2026hframe\x1b[6n"), "");
        let mut replies = Vec::new();
        engine.end_sync(&mut replies);
        assert_eq!(replies, b"\x1b[1;6R");
        assert_eq!(engine.sync_deadline(), None);
    }

    /// Everything of an emulator's state that a preamble claims to bring
    /// back, in a form that can be compared.
    #[derive(Debug, PartialEq)]
    struct Restored {
        mode: alacritty_terminal::term::TermMode,
        cursor: alacritty_terminal::vte::ansi::CursorStyle,
        /// The keyboard stacks of the screen the program is on and of the
        /// other one, read out by asking and popping.
        keyboard: String,
    }

    /// A terminal like the engine's, with the keyboard protocol switched
    /// on so that its stacks can be compared too.
    fn emulator(cols: u16, rows: u16) -> AlacrittyEngine {
        AlacrittyEngine::with_config(
            Config {
                scrolling_history: 0,
                kitty_keyboard: true,
                ..Config::default()
            },
            cols,
            rows,
        )
    }

    fn restored(mut engine: AlacrittyEngine) -> Restored {
        let mut ignored = Vec::new();
        engine.end_sync(&mut ignored);
        let mode = *engine.term().mode();
        let cursor = engine.term().cursor_style();
        let alt = mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
        let mut keyboard = String::new();
        for screen in 0..2 {
            for _ in 0..9 {
                keyboard.push_str(&ask(&mut engine, b"\x1b[?u\x1b[<u"));
            }
            keyboard.push('|');
            let other = if alt == (screen == 0) { 'l' } else { 'h' };
            ask(&mut engine, format!("\x1b[?1049{other}").as_bytes());
        }
        Restored {
            mode,
            cursor,
            keyboard,
        }
    }

    /// The modes the scanner finds at every offset of `stream` where it
    /// may be cut.
    fn cuts(stream: &[u8]) -> Vec<(usize, Arc<Modes>)> {
        let mut scanner = Scanner::new();
        let mut cuts = vec![(0, scanner.snapshot())];
        for (at, byte) in stream.iter().enumerate() {
            if let Some(safe) = scanner.feed(&[*byte]) {
                cuts.push((at + 1, safe.modes));
            }
        }
        cuts
    }

    #[test]
    fn a_preamble_and_the_tail_end_in_the_modes_of_the_whole_stream() {
        // Two mixes, because this emulator does not take `47` and `1047`
        // for the alternate screen and so disagrees with the scanner about
        // which screen a keyboard stack belongs to once they are used; and
        // because mode 1007 after a full reset is whatever the emulator
        // starts with, which a preamble cannot say.
        let mixes = [
            Mix {
                old_screen_modes: false,
                keyboard: true,
                full_reset: true,
                alternate_scroll: false,
            },
            Mix {
                old_screen_modes: true,
                keyboard: false,
                full_reset: false,
                alternate_scroll: true,
            },
        ];
        let mut compared = 0;
        for seed in 1..=12u64 {
            let mix = mixes[(seed % 2) as usize];
            let stream = synthetic(seed, 250, mix).bytes;
            let mut whole = emulator(80, 24);
            let mut ignored = Vec::new();
            whole.advance(&stream, &mut ignored);
            let expected = restored(whole);

            let mut rng = Rng(seed | 1);
            for (cut, modes) in cuts(&stream) {
                // One cut in eight; all of them would take minutes.
                if rng.below(8) != 0 {
                    continue;
                }
                // A client that attaches late: a fresh emulator.
                let mut late = emulator(80, 24);
                late.advance(&modes.preamble(), &mut ignored);
                late.advance(&stream[cut..], &mut ignored);
                assert_eq!(restored(late), expected, "seed {seed}, cut at {cut}");

                // A client that fell behind: an emulator that saw the
                // stream up to anywhere before the cut, the middle of a
                // sequence included, and nothing from there to the cut.
                let seen = rng.below(cut + 1);
                let mut behind = emulator(80, 24);
                behind.advance(&stream[..seen], &mut ignored);
                behind.advance(&modes.preamble(), &mut ignored);
                behind.advance(&stream[cut..], &mut ignored);
                assert_eq!(
                    restored(behind),
                    expected,
                    "seed {seed}, seen up to {seen}, cut at {cut}"
                );
                compared += 2;
            }
        }
        assert!(compared > 500, "only {compared} replays compared");
    }

    #[test]
    fn keyboard_flags_set_in_place_come_back_as_the_flags_in_effect() {
        // Setting flags without pushing is where implementations of the
        // protocol part ways: in the protocol's own, the top of the stack
        // is rewritten, and in this emulator the stack is left alone and a
        // later pop or change of screen forgets the flags. The scanner
        // follows the first, so the generated streams leave the sequence
        // out. The flags in effect right after it are the same in both.
        let stream = b"\x1b[>1u\x1b[=4;2u";
        let modes = cuts(stream).pop().unwrap().1;
        assert_eq!(modes.keyboard_stack(false), [5]);
        let mut ignored = Vec::new();
        let mut whole = emulator(80, 24);
        whole.advance(stream, &mut ignored);
        let mut late = emulator(80, 24);
        late.advance(&modes.preamble(), &mut ignored);
        assert_eq!(late.term().mode(), whole.term().mode());
    }

    /// The resizes that came right before a piece of output, and the piece.
    type Chunk = (Vec<(u16, u16)>, Vec<u8>);

    /// One recorded session: its output and the resizes that fell in it.
    struct Recording {
        name: String,
        size: (u16, u16),
        /// Output in the pieces it was read in, each with the resizes
        /// that came right before it.
        chunks: Vec<Chunk>,
    }

    /// Read a recording in either of the two forms the capture tools
    /// wrote: a log of typed records, or raw output with a description
    /// next to it.
    fn read_recording(path: &std::path::Path) -> Option<Recording> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let data = std::fs::read(path).ok()?;
        if let Some(mut rest) = data.strip_prefix(b"SLOG1\n") {
            let mut size = None;
            let mut chunks = Vec::new();
            let mut resizes = Vec::new();
            while rest.len() >= 13 {
                let kind = rest[0];
                let len = u32::from_le_bytes(rest[9..13].try_into().ok()?) as usize;
                let payload = rest.get(13..13 + len)?;
                rest = &rest[13 + len..];
                match kind {
                    0 => chunks.push((std::mem::take(&mut resizes), payload.to_vec())),
                    1 => {
                        let cols = u16::from_le_bytes(payload.get(0..2)?.try_into().ok()?);
                        let rows = u16::from_le_bytes(payload.get(2..4)?.try_into().ok()?);
                        if size.is_none() && chunks.is_empty() {
                            size = Some((cols, rows));
                        } else {
                            resizes.push((cols, rows));
                        }
                    }
                    // Input and the recording terminal's own answers.
                    _ => {}
                }
            }
            return Some(Recording {
                name,
                size: size.unwrap_or((100, 30)),
                chunks,
            });
        }
        if path.extension()? != "rec" {
            return None;
        }
        let about: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("rec.json")).ok()?).ok()?;
        let number = |value: &serde_json::Value| value.as_u64().map(|n| n as usize);
        let mut chunks = Vec::new();
        for chunk in about.get("chunks")?.as_array()? {
            let (start, len) = (number(chunk.get(1)?)?, number(chunk.get(2)?)?);
            chunks.push((Vec::new(), data.get(start..start + len)?.to_vec()));
        }
        Some(Recording {
            name,
            size: (
                number(about.get("cols")?)? as u16,
                number(about.get("rows")?)? as u16,
            ),
            chunks,
        })
    }

    /// Replays recordings of real sessions through the scanner and a log
    /// small enough to wrap many times, and checks every place the log was
    /// cut: that it was a place the stream may be cut, and that an
    /// emulator given the preamble and the rest of the session ends in the
    /// modes of one that saw the whole session.
    ///
    /// The recordings are not part of the repository. Point
    /// `SPLITLANE_HOST_RECORDINGS` at a directory of them:
    ///
    /// ```text
    /// SPLITLANE_HOST_RECORDINGS=/path cargo test -p splitlane-host --lib \
    ///     recorded_sessions -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs recordings of real sessions, which are kept outside the repository"]
    fn recorded_sessions_survive_every_trim() {
        let Some(dir) = std::env::var_os("SPLITLANE_HOST_RECORDINGS") else {
            eprintln!("SPLITLANE_HOST_RECORDINGS is not set; nothing replayed");
            return;
        };
        let capacity: usize = std::env::var("SPLITLANE_HOST_RECORDINGS_RING")
            .ok()
            .and_then(|ring| ring.parse().ok())
            .unwrap_or(2048);
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .collect();
        paths.sort();
        let mut totals = [0usize; 6];
        let mut failures = Vec::new();
        for path in paths {
            let Some(recording) = read_recording(&path) else {
                continue;
            };
            let (cols, rows) = recording.size;
            let whole: Vec<u8> = recording
                .chunks
                .iter()
                .flat_map(|(_, bytes)| bytes.iter().copied())
                .collect();
            let safe: Vec<usize> = cuts(&whole).into_iter().map(|(at, _)| at).collect();
            let in_update = {
                // How much of the session a client could not have been cut
                // into at all.
                let mut scanner = Scanner::new();
                whole
                    .iter()
                    .filter(|byte| {
                        scanner.feed(&[**byte]);
                        !scanner.is_safe()
                    })
                    .count()
            };

            let mut ignored = Vec::new();
            let mut full = emulator(cols, rows);
            for (resizes, bytes) in &recording.chunks {
                for (cols, rows) in resizes {
                    full.resize(*cols, *rows, None);
                }
                full.advance(bytes, &mut ignored);
            }
            let expected = restored(full);

            let mut log = SessionLog::new(capacity, cols, rows);
            let mut resize_seqs: Vec<u64> = Vec::new();
            let (mut trims, mut torn, mut unsafe_cuts, mut mismatches) = (0, 0, 0, 0);
            let mut largest = 0;
            for (index, (resizes, bytes)) in recording.chunks.iter().enumerate() {
                let before = log.head_seq();
                for (cols, rows) in resizes {
                    resize_seqs.push(log.tail_seq());
                    log.append_resize(*cols, *rows);
                }
                log.append_output(bytes);
                largest = largest.max(log.stored_bytes());
                if log.head_seq() == before {
                    continue;
                }
                trims += 1;
                let head = log.head_seq();
                let offset =
                    (head - resize_seqs.iter().filter(|seq| **seq < head).count() as u64) as usize;
                // A torn head is the log saying it had no better place;
                // a head it calls clean has to be one.
                torn += usize::from(log.head_torn());
                if !log.head_torn() && safe.binary_search(&offset).is_err() {
                    unsafe_cuts += 1;
                    failures.push(format!(
                        "{}: the cut at offset {offset} is called clean and is not",
                        recording.name
                    ));
                }

                // What a client attaching right now would be sent, and
                // then everything the session went on to print.
                let (cols, rows) = log.size_at(head);
                let mut late = emulator(cols, rows);
                late.advance(&log.head_preamble(), &mut ignored);
                let mut next = head;
                loop {
                    let batch = log.read_from(next, 64 * 1024);
                    if batch.items.is_empty() {
                        break;
                    }
                    for item in &batch.items {
                        match item {
                            Item::Output { bytes, .. } => late.advance(bytes, &mut ignored),
                            Item::Resize { cols, rows, .. } => late.resize(*cols, *rows, None),
                        }
                    }
                    next = batch.next_seq;
                }
                // The session is not over at this trim; what the log does
                // not hold yet comes from the recording.
                for (resizes, bytes) in &recording.chunks[index + 1..] {
                    for (cols, rows) in resizes {
                        late.resize(*cols, *rows, None);
                    }
                    late.advance(bytes, &mut ignored);
                }
                let got = restored(late);
                if got != expected {
                    mismatches += 1;
                    failures.push(format!(
                        "{}: after the cut at offset {offset}: {got:?}, the whole session: {expected:?}",
                        recording.name
                    ));
                }
            }
            eprintln!(
                "{:<24} {:>7} bytes ({:>3}% not cuttable), {:>3} resizes, {:>4} trims, {} torn, {} unsafe, {} mode mismatches, log at most {} bytes",
                recording.name,
                whole.len(),
                in_update * 100 / whole.len().max(1),
                resize_seqs.len(),
                trims,
                torn,
                unsafe_cuts,
                mismatches,
                largest,
            );
            for (total, add) in
                totals
                    .iter_mut()
                    .zip([1, whole.len(), trims, torn, unsafe_cuts, mismatches])
            {
                *total += add;
            }
        }
        eprintln!(
            "total: {} recordings, {} bytes, ring {capacity}: {} trims, {} torn, {} unsafe, {} mode mismatches",
            totals[0], totals[1], totals[2], totals[3], totals[4], totals[5]
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
