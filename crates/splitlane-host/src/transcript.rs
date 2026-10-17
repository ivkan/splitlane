//! A session's text, rebuilt from its log for a reader.
//!
//! The engine that answers the program keeps almost no history: on macOS
//! none, on Linux the few lines that put the cursor where a pane has it.
//! A reader with no window - `splitlane read` with Splitlane closed - wants
//! the history above the screen too, as a pane gives it. So the text is not
//! taken from that engine. The log is replayed into an emulator of its own,
//! made for the one read, with history, and dropped after it: the engine the
//! program talks to is never touched, and what one read costs is a replay,
//! paid only when somebody reads.
//!
//! The emulator is the platform's pane emulator, so the text matches what a
//! pane would show for the same output: Alacritty's on macOS, libghostty on
//! Linux. Where the host has no engine it has no emulator here either.

use crate::log::Item;

/// Most lines of text a read returns, oldest dropped first: what a pane
/// reads at most for `surface.read`.
pub const MAX_LINES: usize = 4000;

/// Why a session's text could not be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// This build of the host carries no emulator.
    NoEmulator,
    /// The emulator could not be made or failed while replaying.
    Failed,
}

/// Replay `preamble` and then `items` into a terminal of `cols` x `rows`
/// and return its history and screen as text, most recent line last,
/// trailing blank lines and spaces trimmed. `None` for a terminal that
/// shows nothing.
pub fn transcribe(
    cols: u16,
    rows: u16,
    preamble: &[u8],
    items: &[Item],
) -> Result<Option<String>, Unavailable> {
    #[cfg(target_os = "macos")]
    {
        Ok(alacritty::transcribe(cols, rows, preamble, items))
    }
    #[cfg(all(target_os = "linux", feature = "libghostty-linux"))]
    {
        ghostty::transcribe(cols, rows, preamble, items)
    }
    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "linux", feature = "libghostty-linux")
    )))]
    {
        let _ = (cols, rows, preamble, items);
        Err(Unavailable::NoEmulator)
    }
}

/// Join the rows, newest last, and trim what a pane's reader trims.
#[cfg_attr(
    not(any(
        target_os = "macos",
        all(target_os = "linux", feature = "libghostty-linux")
    )),
    allow(dead_code)
)]
fn finish(mut lines: Vec<String>) -> Option<String> {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.len() > MAX_LINES {
        lines.drain(..lines.len() - MAX_LINES);
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(target_os = "macos")]
mod alacritty {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use crate::log::Item;

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

    fn size(cols: u16, rows: u16) -> Size {
        Size {
            cols: cols.max(2),
            rows: rows.max(1),
        }
    }

    pub(super) fn transcribe(
        cols: u16,
        rows: u16,
        preamble: &[u8],
        items: &[Item],
    ) -> Option<String> {
        let config = Config {
            scrolling_history: super::MAX_LINES,
            ..Config::default()
        };
        let mut term = Term::new(config, &size(cols, rows), VoidListener);
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut term, preamble);
        for item in items {
            match item {
                Item::Output { bytes, .. } => processor.advance(&mut term, bytes),
                Item::Resize { cols, rows, .. } => term.resize(size(*cols, *rows)),
            }
        }
        // Output held back for a synchronized update the program has not
        // ended is still output it printed.
        processor.stop_sync(&mut term);

        let top = term.topmost_line().0;
        let end = term.bottommost_line().0 + 1;
        let last = term.last_column();
        let lines = (top..end)
            .map(|row| {
                term.bounds_to_string(
                    Point::new(Line(row), Column(0)),
                    Point::new(Line(row), last),
                )
                .trim_end()
                .to_string()
            })
            .collect();
        super::finish(lines)
    }
}

#[cfg(all(target_os = "linux", feature = "libghostty-linux"))]
mod ghostty {
    use splitlane_terminal_ghostty::{DisplayTerminal, WindowSize};

    use super::Unavailable;
    use crate::log::Item;

    fn size(cols: u16, rows: u16) -> Result<WindowSize, Unavailable> {
        WindowSize::new(usize::from(cols.max(1)), usize::from(rows.max(1)), 0, 0)
            .map_err(|_| Unavailable::Failed)
    }

    /// Made, used and dropped on the calling thread, which is all the
    /// terminal asks: it may not move between threads.
    pub(super) fn transcribe(
        cols: u16,
        rows: u16,
        preamble: &[u8],
        items: &[Item],
    ) -> Result<Option<String>, Unavailable> {
        let failed = |_| Unavailable::Failed;
        let mut terminal =
            DisplayTerminal::new(size(cols, rows)?, super::MAX_LINES).map_err(failed)?;
        terminal.feed(preamble).map_err(failed)?;
        for item in items {
            match item {
                Item::Output { bytes, .. } => terminal.feed(bytes).map_err(failed)?,
                Item::Resize { cols, rows, .. } => {
                    terminal.resize(size(*cols, *rows)?).map_err(failed)?
                }
            }
            // Its events are for a window; left queued they only grow.
            drop(terminal.drain_events());
        }
        let text = terminal.extract_text().map_err(failed)?;
        Ok(text.and_then(|text| super::finish(text.split('\n').map(str::to_string).collect())))
    }
}

#[cfg(test)]
#[cfg(any(
    target_os = "macos",
    all(target_os = "linux", feature = "libghostty-linux")
))]
mod tests {
    use super::*;

    fn output(text: &str) -> Item {
        Item::Output {
            seq: 0,
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn the_history_above_the_screen_is_read_too() {
        let lines: String = (1..=30).map(|n| format!("line {n}\r\n")).collect();
        let text = transcribe(20, 5, b"", &[output(&lines)]).unwrap().unwrap();
        assert!(text.starts_with("line 1\nline 2\n"), "{text}");
        assert!(text.ends_with("line 30"), "{text}");
    }

    #[test]
    fn a_blank_terminal_reads_as_nothing() {
        assert_eq!(transcribe(20, 5, b"", &[]).unwrap(), None);
        assert_eq!(transcribe(20, 5, b"", &[output("\r\n\r\n")]).unwrap(), None);
    }

    #[test]
    fn a_program_that_redraws_its_screen_reads_as_it_was_left() {
        // Clear and draw again, as a full-screen program does.
        let text = transcribe(
            20,
            5,
            b"",
            &[output("first\r\n"), output("\x1b[2J\x1b[Hsecond")],
        )
        .unwrap()
        .unwrap();
        assert!(text.ends_with("second"), "{text}");
    }

    #[test]
    fn a_resize_in_the_log_is_replayed_where_it_happened() {
        let items = [
            output("abcdefghij"),
            Item::Resize {
                seq: 10,
                cols: 40,
                rows: 5,
            },
            output("\r\n0123456789012345678901234567890123456789"),
        ];
        let text = transcribe(10, 5, b"", &items).unwrap().unwrap();
        assert!(
            text.ends_with("0123456789012345678901234567890123456789"),
            "{text}"
        );
    }
}
