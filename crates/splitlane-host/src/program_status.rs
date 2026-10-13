//! The program status protocol, OSC 7501: a program says what it is doing.
//!
//! A program that knows whether it is working, finished or waiting on a
//! person can print that as an escape sequence, and several coding agents
//! do. They only start after asking whether anybody reads it, so there are
//! two halves here: the [`Scanner`] finds the question and the reports in a
//! stream of output, and [`reply`] is the answer to the question.
//!
//! This lives beside the host's other scanner because both ends need it.
//! The host answers the question for a session it owns, which is the only
//! place that can while no window is attached; the window reads the reports,
//! and answers for a pane whose PTY it holds itself.
//!
//! Everything in a report is untrusted: any program that can print to the
//! pane can print one, and so can a file somebody runs `cat` on. Nothing
//! here decides what a report is worth. It only refuses the ones the
//! protocol says to refuse, whole, so that no half of a bad report is ever
//! applied.
//!
//! Only the question and the root record are of use to the caller today, but
//! a report for a child record is still parsed and handed over with its id:
//! dropping it here would make a child's `clear` indistinguishable from one
//! that empties the terminal.

/// Longest sequence the protocol allows, from its introducer through its
/// terminator.
pub const SEQUENCE_MAX: usize = 4096;

/// Longest body: what is left of [`SEQUENCE_MAX`] after `ESC ] 7501 ;` and
/// the shortest terminator.
const BODY_MAX: usize = SEQUENCE_MAX - 8;

const KEY_MAX: usize = 16;
const APP_MAX: usize = 32;
const ID_MAX: usize = 128;
const ID_SEGMENT_MAX: usize = 32;
const ID_DEPTH_MAX: usize = 8;
const MSG_ENCODED_MAX: usize = 2732;
const MSG_DECODED_MAX: usize = 2048;
const TITLE_ENCODED_MAX: usize = 256;
const TITLE_DECODED_MAX: usize = 192;

const PREFIX: &[u8] = b"7501;";

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// The answer to the question, ending the way the question did.
///
/// It lists the states and kinds although they are the protocol's defaults,
/// and the reason is the list's length, not its content. The bare answer is
/// byte for byte the question. A program that asks and exits leaves the
/// answer to whatever reads the terminal next, and a line discipline that
/// echoes it raw would hand the question straight back, for ever. With the
/// list the echo is no longer a question.
pub fn reply(bel: bool) -> &'static [u8] {
    if bel {
        b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07"
    } else {
        b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\"
    }
}

/// What a program says it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// At rest, waiting for the next instruction.
    Idle,
    Working,
    /// Finished a piece of work nobody has looked at yet.
    Done,
    /// Cannot continue until a person does something.
    Blocked,
    /// Failed and stopped.
    Error,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Error => "error",
        }
    }

    /// Whether a record in this state ends with the program that made it.
    /// The other states are left behind for a person to find.
    pub fn ends_with_the_program(self) -> bool {
        matches!(self, State::Working | State::Blocked)
    }
}

/// What a blocked program is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Approval to do something.
    Permission,
    /// An answer that has to be typed.
    Question,
    /// A login, a token or a credential.
    Auth,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Auth => "auth",
        }
    }
}

/// One record as its last report left it. A report replaces its record
/// completely, so a field that is `None` was absent from that report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub state: State,
    /// Only ever set on a blocked record.
    pub kind: Option<Kind>,
    /// A percentage, only ever set on a working or blocked record.
    pub progress: Option<u8>,
    /// The program's stable identifier. Its own claim, like the rest.
    pub app: Option<String>,
    pub title: Option<String>,
    /// One line of text, free of control characters.
    pub msg: Option<String>,
}

/// What a report does to the record it addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Set(Record),
    /// Remove the record and every record beneath it; with no id, every
    /// record of the terminal.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The record addressed. `None` is the root record, the only one a
    /// program that reports just its own state uses.
    pub id: Option<String>,
    pub change: Change,
}

/// What one OSC 7501 sequence turned out to be. The report borrows the
/// scanner's buffer; [`parse`] makes a [`Report`] of it.
#[derive(Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// The program asked whether its reports are read. `bel` is how the
    /// question ended, for [`reply`].
    Query {
        bel: bool,
    },
    Report(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Ground,
    Esc,
    Prefix(u8),
    Body,
    BodyEsc,
    Skip,
    SkipEsc,
}

/// Finds OSC 7501 sequences in a stream of output, across any chunking.
///
/// Runs on the thread that reads the PTY, on every byte, so it allocates
/// once, when it is made. A sequence longer than the protocol allows is
/// dropped, not cut.
#[derive(Debug)]
pub struct Scanner {
    state: ScanState,
    body: Vec<u8>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            state: ScanState::Ground,
            body: Vec::with_capacity(BODY_MAX),
        }
    }

    pub fn feed(&mut self, bytes: &[u8], on_event: &mut impl FnMut(Event<'_>)) {
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            match self.state {
                ScanState::Ground => {
                    // Most output is not an escape sequence and is passed
                    // over as a run.
                    let Some(offset) = bytes[index..].iter().position(|byte| *byte == ESC) else {
                        return;
                    };
                    index += offset + 1;
                    self.state = ScanState::Esc;
                    continue;
                }
                ScanState::Esc => {
                    self.state = match byte {
                        b']' => ScanState::Prefix(0),
                        ESC => ScanState::Esc,
                        _ => ScanState::Ground,
                    };
                }
                ScanState::Prefix(matched) => {
                    self.state = if byte == PREFIX[matched as usize] {
                        if matched as usize + 1 == PREFIX.len() {
                            self.body.clear();
                            ScanState::Body
                        } else {
                            ScanState::Prefix(matched + 1)
                        }
                    } else {
                        match byte {
                            BEL | CAN | SUB => ScanState::Ground,
                            ESC => ScanState::SkipEsc,
                            _ => ScanState::Skip,
                        }
                    };
                }
                ScanState::Body => match byte {
                    BEL => {
                        self.emit(true, on_event);
                        self.state = ScanState::Ground;
                    }
                    ESC => self.state = ScanState::BodyEsc,
                    CAN | SUB => self.state = ScanState::Ground,
                    _ if self.body.len() < BODY_MAX => self.body.push(byte),
                    _ => self.state = ScanState::Skip,
                },
                ScanState::BodyEsc => {
                    self.state = match byte {
                        b'\\' => {
                            self.emit(false, on_event);
                            ScanState::Ground
                        }
                        // The string was abandoned for a new sequence.
                        b']' => ScanState::Prefix(0),
                        ESC => ScanState::Esc,
                        _ => ScanState::Ground,
                    };
                }
                ScanState::Skip => match byte {
                    BEL | CAN | SUB => self.state = ScanState::Ground,
                    ESC => self.state = ScanState::SkipEsc,
                    _ => {}
                },
                ScanState::SkipEsc => {
                    self.state = match byte {
                        b']' => ScanState::Prefix(0),
                        ESC => ScanState::Esc,
                        _ => ScanState::Ground,
                    };
                }
            }
            index += 1;
        }
    }

    fn emit(&mut self, bel: bool, on_event: &mut impl FnMut(Event<'_>)) {
        // Only the bare question is one. What a terminal answers begins the
        // same way and goes on; see `reply` for why that must not be
        // answered in turn.
        if self.body == b"?" {
            on_event(Event::Query { bel });
        } else if self.body.first() != Some(&b'?') {
            on_event(Event::Report(&self.body));
        }
    }
}

/// Read the body of a report. `None` for one the protocol says to ignore or
/// to discard whole.
pub fn parse(body: &[u8]) -> Option<Report> {
    let mut state = None;
    let mut id = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut title = None;
    let mut msg = None;
    for pair in body.split(|byte| *byte == b':') {
        let Some(at) = pair.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (key, value) = (pair[..at].trim_ascii(), pair[at + 1..].trim_ascii());
        if key.len() > KEY_MAX {
            return None;
        }
        if key.is_empty() || !key.iter().all(u8::is_ascii_lowercase) {
            continue;
        }
        if !value.iter().all(|byte| is_value_byte(*byte)) {
            // A malformed pair is passed over and the rest still counts,
            // except where that would re-address the report: one whose id
            // cannot be read must not land on the root record.
            if key == b"id" {
                return None;
            }
            continue;
        }
        // Kept as written and read after the loop: when a key repeats the
        // last value wins, so an earlier one is never judged.
        match key {
            b"state" => state = Some(value),
            b"id" => id = Some(value),
            b"kind" => kind = Some(value),
            b"progress" => progress = Some(value),
            b"app" => app = Some(value),
            b"title" => title = Some(value),
            b"msg" => msg = Some(value),
            _ => {}
        }
    }

    // An id that is not one makes the report nobody's: falling back to the
    // root record would let a malformed id overwrite it.
    let id = match id {
        Some(id) => Some(valid_id(id)?),
        None => None,
    };
    let state = match state? {
        b"clear" => {
            return Some(Report {
                id,
                change: Change::Clear,
            })
        }
        b"idle" => State::Idle,
        b"working" => State::Working,
        b"done" => State::Done,
        b"blocked" => State::Blocked,
        b"error" => State::Error,
        // A state added later must not turn into another one here.
        _ => return None,
    };
    let kind = match kind {
        Some(b"permission") => Some(Kind::Permission),
        Some(b"question") => Some(Kind::Question),
        Some(b"auth") => Some(Kind::Auth),
        _ => None,
    }
    .filter(|_| state == State::Blocked);
    let progress = progress
        .and_then(|value| std::str::from_utf8(value).ok()?.parse::<u8>().ok())
        .filter(|percent| *percent <= 100 && state.ends_with_the_program());
    let app = match app {
        Some(app) if app.len() > APP_MAX => return None,
        Some(app) if !app.is_empty() && app.iter().all(|byte| is_name_byte(*byte)) => {
            Some(String::from_utf8_lossy(app).into_owned())
        }
        _ => None,
    };
    let title = match title {
        Some(title) => Some(text(title, TITLE_ENCODED_MAX, TITLE_DECODED_MAX)?),
        None => None,
    };
    let msg = match msg {
        Some(msg) => Some(text(msg, MSG_ENCODED_MAX, MSG_DECODED_MAX)?),
        None => None,
    };
    Some(Report {
        id,
        change: Change::Set(Record {
            state,
            kind,
            progress,
            app,
            title,
            msg,
        }),
    })
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte)
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte)
}

fn valid_id(id: &[u8]) -> Option<String> {
    if id.len() > ID_MAX {
        return None;
    }
    let mut depth = 0;
    for segment in id.split(|byte| *byte == b'/') {
        depth += 1;
        if segment.is_empty()
            || segment.len() > ID_SEGMENT_MAX
            || !segment.iter().all(|byte| is_name_byte(*byte))
        {
            return None;
        }
    }
    (depth <= ID_DEPTH_MAX).then(|| String::from_utf8_lossy(id).into_owned())
}

/// A free-text value: base64 of one line of UTF-8. `None` when it is too
/// long, does not decode, or holds a control character, each of which
/// discards the report it came in.
fn text(encoded: &[u8], encoded_max: usize, decoded_max: usize) -> Option<String> {
    if encoded.len() > encoded_max {
        return None;
    }
    let decoded = String::from_utf8(base64(encoded)?).ok()?;
    if decoded.len() > decoded_max || decoded.chars().any(char::is_control) {
        return None;
    }
    Some(decoded)
}

/// Standard base64, padding optional. Small enough to keep here, and the
/// host is the one process that should not grow a dependency for it.
fn base64(encoded: &[u8]) -> Option<Vec<u8>> {
    let unpadded = match encoded {
        [rest @ .., b'=', b'='] | [rest @ .., b'='] => rest,
        all => all,
    };
    // One sextet alone carries less than a byte.
    if unpadded.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(unpadded.len() / 4 * 3 + 2);
    for group in unpadded.chunks(4) {
        let mut bits = 0u32;
        for byte in group {
            let sextet = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            };
            bits = bits << 6 | u32::from(sextet);
        }
        bits <<= 6 * (4 - group.len());
        let bytes = bits.to_be_bytes();
        out.extend_from_slice(&bytes[1..group.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Seen {
        Query { bel: bool },
        Report(Vec<u8>),
    }

    fn scan(chunks: &[&[u8]]) -> Vec<Seen> {
        let mut scanner = Scanner::new();
        let mut seen = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, &mut |event| {
                seen.push(match event {
                    Event::Query { bel } => Seen::Query { bel },
                    Event::Report(body) => Seen::Report(body.to_vec()),
                });
            });
        }
        seen
    }

    fn set(body: &str) -> Record {
        match parse(body.as_bytes()) {
            Some(Report {
                id: None,
                change: Change::Set(record),
            }) => record,
            other => unreachable!("{body:?} is not a root record: {other:?}"),
        }
    }

    /// What Claude Code 2.1.296 printed on a Bash permission prompt.
    const PERMISSION: &str =
        "state=blocked:kind=permission:app=claude-code:msg=YXBwcm92ZSBCYXNoOiBlY2hvIGhp";

    #[test]
    fn a_recorded_permission_prompt_reads_as_what_it_said() {
        assert_eq!(
            set(PERMISSION),
            Record {
                state: State::Blocked,
                kind: Some(Kind::Permission),
                progress: None,
                app: Some("claude-code".into()),
                title: None,
                msg: Some("approve Bash: echo hi".into()),
            }
        );
    }

    #[test]
    fn the_question_and_a_report_are_found_with_either_terminator() {
        assert_eq!(
            scan(&[b"text\x1b]7501;?\x1b\\more\x1b]7501;?\x07\x1b]7501;state=idle\x07"]),
            [
                Seen::Query { bel: false },
                Seen::Query { bel: true },
                Seen::Report(b"state=idle".to_vec()),
            ]
        );
    }

    #[test]
    fn a_sequence_is_found_wherever_the_stream_is_cut() {
        let stream = b"a\x1b[0m\x1b]7501;state=working:app=x\x1b\\b\x1b]7501;?\x1b\\";
        let whole = scan(&[stream]);
        assert_eq!(whole.len(), 2);
        for cut in 1..stream.len() {
            assert_eq!(
                scan(&[&stream[..cut], &stream[cut..]]),
                whole,
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn other_sequences_are_not_reports() {
        assert_eq!(
            scan(&[b"\x1b]0;7501;state=idle\x07\x1b]75010;state=idle\x07\x1b]750;x\x07"]),
            []
        );
        // One that was cut short by the next still lets the next be found.
        assert_eq!(
            scan(&[b"\x1b]0;title\x1b]7501;state=done\x07"]),
            [Seen::Report(b"state=done".to_vec())]
        );
        assert_eq!(
            scan(&[b"\x1b]7501;state=idle\x18\x1b]7501;state=done\x1a"]),
            []
        );
    }

    #[test]
    fn an_answer_is_not_a_question() {
        assert_eq!(scan(&[reply(false), reply(true)]), []);
    }

    #[test]
    fn a_sequence_past_the_limit_is_dropped_and_the_next_is_read() {
        let mut stream = b"\x1b]7501;state=idle:msg=".to_vec();
        stream.resize(stream.len() + SEQUENCE_MAX, b'A');
        stream.extend_from_slice(b"\x07\x1b]7501;state=done\x07");
        assert_eq!(scan(&[&stream]), [Seen::Report(b"state=done".to_vec())]);
    }

    #[test]
    fn a_report_without_a_state_it_knows_is_ignored() {
        assert_eq!(parse(b"app=x"), None);
        assert_eq!(parse(b"state=paused:app=x"), None);
        assert_eq!(parse(b""), None);
    }

    #[test]
    fn a_malformed_pair_is_passed_over_and_the_rest_counts() {
        let record =
            set("nonsense:=x:Key=1:state=working:app=a b:title=a$b:progress=40: msg = aGk ");
        assert_eq!(record.state, State::Working);
        assert_eq!(record.progress, Some(40));
        assert_eq!((record.app, record.title), (None, None));
        assert_eq!(record.msg.as_deref(), Some("hi"));
    }

    #[test]
    fn the_last_of_a_repeated_key_wins() {
        assert_eq!(set("state=idle:state=done").state, State::Done);
        // The earlier value would have discarded the report.
        assert_eq!(
            set("state=idle:msg=!!!:msg=aGk=").msg.as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn kind_and_progress_belong_to_the_states_that_carry_them() {
        let done = set("state=done:kind=question:progress=50");
        assert_eq!((done.kind, done.progress), (None, None));
        let blocked = set("state=blocked:kind=later:progress=101");
        assert_eq!((blocked.kind, blocked.progress), (None, None));
        let blocked = set("state=blocked:kind=auth:progress=100");
        assert_eq!(
            (blocked.kind, blocked.progress),
            (Some(Kind::Auth), Some(100))
        );
    }

    #[test]
    fn text_that_cannot_be_shown_discards_the_whole_report() {
        // "a\nb", a C1 control, bytes that are not UTF-8, not base64.
        for msg in ["YQpi", "wps=", "/w==", "a", "a.b"] {
            assert_eq!(
                parse(format!("state=idle:msg={msg}").as_bytes()),
                None,
                "{msg}"
            );
        }
        let long = "QUFB".repeat(MSG_DECODED_MAX / 3 + 1);
        assert_eq!(parse(format!("state=idle:msg={long}").as_bytes()), None);
        assert_eq!(
            parse(format!("state=idle:app={}", "a".repeat(33)).as_bytes()),
            None
        );
    }

    #[test]
    fn padding_is_optional() {
        for msg in ["aGk", "aGk=", "aGV5", "aA", "aA=="] {
            assert!(set(&format!("state=idle:msg={msg}")).msg.is_some(), "{msg}");
        }
    }

    #[test]
    fn an_id_addresses_a_record_of_its_own_or_nothing() {
        assert_eq!(
            parse(b"state=clear:id=build/test"),
            Some(Report {
                id: Some("build/test".into()),
                change: Change::Clear,
            })
        );
        assert_eq!(
            parse(b"state=clear"),
            Some(Report {
                id: None,
                change: Change::Clear,
            })
        );
        let deep = ["a"; ID_DEPTH_MAX + 1].join("/");
        for id in ["", "a//b", "/a", "a b", &deep, &"a".repeat(33)] {
            assert_eq!(
                parse(format!("state=idle:id={id}").as_bytes()),
                None,
                "{id:?}"
            );
        }
    }
}
