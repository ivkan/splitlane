//! The wire contract between the session host and whatever attaches to it.
//!
//! A host outlives the window that started it, so the two ends of a
//! connection are routinely different releases. Nothing here may therefore
//! depend on both ends being built from the same source. The rules:
//!
//! # Framing
//!
//! Every frame is `u32` little-endian length, one byte of frame kind, then
//! the body. The length counts the kind byte and the body, so it is never
//! zero. A frame longer than [`MAX_FRAME_LEN`] is refused before anything is
//! allocated for it and the connection is closed.
//!
//! Control frames ([`KIND_HELLO`], [`KIND_REQUEST`], [`KIND_REPLY`],
//! [`KIND_EVENT`], [`KIND_ERROR`]) carry one JSON object. Data frames are
//! binary, because terminal output is not text and is most of the traffic:
//!
//! - [`KIND_OUTPUT`]: 16-byte session id, `u64` little-endian log sequence
//!   number of the first byte, then the bytes.
//! - [`KIND_INPUT`]: 16-byte session id, then the bytes. It has no reply.
//!
//! # Handshake
//!
//! The client sends a [`Hello`], the host answers with its own. Each side
//! states the contract it speaks (`contract`) and the oldest one it still
//! accepts (`min_contract`). Two sides are compatible when each side's
//! `contract` is at least the other's `min_contract`; see [`negotiate`].
//! Release versions are carried for display and are never compared.
//!
//! # What may change without raising [`MIN_CONTRACT`]
//!
//! - A new field in any JSON object. Every reader ignores fields it does not
//!   know and every optional field has a default, so an older peer simply
//!   does not see it.
//! - A new request `op`. A host that does not know it answers that request
//!   with the error code `unknown_op` and keeps the connection.
//! - A new `event` name or a new frame kind that the receiver may drop
//!   without losing track of the stream. Unknown events and unknown frame
//!   kinds are skipped.
//! - Raising [`CONTRACT`] to announce any of the above, so a peer can tell
//!   whether the other side has it.
//!
//! # What may not
//!
//! - The framing, the meaning of a frame kind number, or the layout of a
//!   data frame.
//! - Removing or renaming a field, an `op` or an `event`, or changing the
//!   type or meaning of one.
//! - What a sequence number counts: one per byte of output and one per
//!   resize record, in arrival order.
//! - Anything a peer must understand to stay correct. That needs a new frame
//!   kind or `op` plus a raised [`MIN_CONTRACT`], which cuts off every older
//!   peer and so every session it was holding.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// The contract this build speaks.
pub const CONTRACT: u32 = 1;
/// The oldest contract this build still accepts from a peer.
pub const MIN_CONTRACT: u32 = 1;

/// Upper bound on the length field of a frame.
pub const MAX_FRAME_LEN: usize = 1024 * 1024;
/// Most bytes one data frame carries. Far below [`MAX_FRAME_LEN`], so a
/// stream of output never holds the connection's writer for long.
pub const MAX_DATA_CHUNK: usize = 64 * 1024;

pub const KIND_HELLO: u8 = 1;
pub const KIND_REQUEST: u8 = 2;
pub const KIND_REPLY: u8 = 3;
pub const KIND_EVENT: u8 = 4;
pub const KIND_ERROR: u8 = 5;
pub const KIND_OUTPUT: u8 = 16;
pub const KIND_INPUT: u8 = 17;

pub const OP_SPAWN: &str = "spawn";
pub const OP_LIST: &str = "list";
pub const OP_ATTACH: &str = "attach";
pub const OP_DETACH: &str = "detach";
pub const OP_RESIZE: &str = "resize";
pub const OP_KILL: &str = "kill";
pub const OP_FORGET: &str = "forget";
pub const OP_SHUTDOWN: &str = "shutdown";

pub const EVENT_RESIZED: &str = "resized";
pub const EVENT_EXITED: &str = "exited";
pub const EVENT_GAP: &str = "gap";

pub const ERR_INCOMPATIBLE: &str = "incompatible";
pub const ERR_PROTOCOL: &str = "protocol";
pub const ERR_FORBIDDEN: &str = "forbidden";
pub const ERR_BUSY: &str = "busy";
pub const ERR_UNKNOWN_OP: &str = "unknown_op";
pub const ERR_BAD_REQUEST: &str = "bad_request";
pub const ERR_NO_SESSION: &str = "no_session";
pub const ERR_SESSION_LIVE: &str = "session_live";
pub const ERR_SESSION_EXITED: &str = "session_exited";
pub const ERR_LIVE_SESSIONS: &str = "live_sessions";
pub const ERR_SPAWN: &str = "spawn_failed";
pub const ERR_ALREADY_ATTACHED: &str = "already_attached";

const HEADER_LEN: usize = 5;
const SESSION_ID_LEN: usize = 16;

/// Identifies one session for as long as the host that owns it lives.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub [u8; SESSION_ID_LEN]);

impl SessionId {
    pub fn new_random() -> Self {
        Self(*uuid::Uuid::new_v4().as_bytes())
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        uuid::Uuid::from_bytes(self.0).hyphenated().fmt(f)
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(s).map(|id| Self(*id.as_bytes()))
    }
}

impl Serialize for SessionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Why a frame could not be read.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The peer closed the connection between two frames.
    #[error("connection closed")]
    Closed,
    /// The caller's `keep_waiting` said to stop.
    #[error("stopped waiting for a frame")]
    Stopped,
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN}-byte limit")]
    Oversized(usize),
    #[error("frame with a zero length")]
    Empty,
    #[error("malformed frame: {0}")]
    Malformed(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One frame as read off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub body: Vec<u8>,
}

/// Write one frame whose body is the concatenation of `parts`.
///
/// The frame is assembled first and written with a single `write_all`, so a
/// writer shared behind a lock never interleaves two frames.
pub fn write_frame<W: Write>(writer: &mut W, kind: u8, parts: &[&[u8]]) -> io::Result<()> {
    let body_len: usize = parts.iter().map(|part| part.len()).sum();
    let len = body_len + 1;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame of {len} bytes exceeds the {MAX_FRAME_LEN}-byte limit"),
        ));
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + body_len);
    frame.extend_from_slice(&(len as u32).to_le_bytes());
    frame.push(kind);
    for part in parts {
        frame.extend_from_slice(part);
    }
    writer.write_all(&frame)?;
    writer.flush()
}

/// Write a control frame carrying `value` as JSON.
pub fn write_json<W: Write, T: Serialize>(writer: &mut W, kind: u8, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    write_frame(writer, kind, &[&body])
}

/// Read one frame, blocking until it is complete.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Frame, FrameError> {
    read_frame_patiently(reader, || true)
}

/// Read one frame from a reader that has a receive timeout.
///
/// Each time the reader times out, `keep_waiting` decides whether to go on.
/// Bytes already received are kept across timeouts, so a frame that arrives
/// slowly is still read whole; that is what lets a connection thread check a
/// stop flag without ever cutting a frame in half.
pub fn read_frame_patiently<R: Read>(
    reader: &mut R,
    mut keep_waiting: impl FnMut() -> bool,
) -> Result<Frame, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    fill(reader, &mut header, true, &mut keep_waiting)?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len == 0 {
        return Err(FrameError::Empty);
    }
    // Checked before the body is allocated: the length is whatever the peer
    // wrote, and four bytes must not be able to reserve four gigabytes.
    if len > MAX_FRAME_LEN {
        return Err(FrameError::Oversized(len));
    }
    let mut body = vec![0u8; len - 1];
    fill(reader, &mut body, false, &mut keep_waiting)?;
    Ok(Frame {
        kind: header[4],
        body,
    })
}

fn fill<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    at_frame_start: bool,
    keep_waiting: &mut impl FnMut() -> bool,
) -> Result<(), FrameError> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 && at_frame_start => return Err(FrameError::Closed),
            Ok(0) => {
                return Err(FrameError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed in the middle of a frame",
                )))
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if !keep_waiting() {
                    return Err(FrameError::Stopped);
                }
            }
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    Ok(())
}

/// Decode the JSON body of a control frame.
pub fn decode_json<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, FrameError> {
    serde_json::from_slice(body).map_err(|e| FrameError::Malformed(e.to_string()))
}

/// Body of an [`KIND_OUTPUT`] frame, borrowed from the frame.
#[derive(Debug, PartialEq, Eq)]
pub struct OutputFrame<'a> {
    pub session: SessionId,
    pub seq: u64,
    pub bytes: &'a [u8],
}

pub fn write_output<W: Write>(
    writer: &mut W,
    session: SessionId,
    seq: u64,
    bytes: &[u8],
) -> io::Result<()> {
    write_frame(
        writer,
        KIND_OUTPUT,
        &[&session.0, &seq.to_le_bytes(), bytes],
    )
}

pub fn decode_output(body: &[u8]) -> Result<OutputFrame<'_>, FrameError> {
    let (session, rest) = split_session(body)?;
    let Some((seq, bytes)) = rest.split_first_chunk::<8>() else {
        return Err(FrameError::Malformed(
            "output frame without a sequence".into(),
        ));
    };
    Ok(OutputFrame {
        session,
        seq: u64::from_le_bytes(*seq),
        bytes,
    })
}

pub fn write_input<W: Write>(writer: &mut W, session: SessionId, bytes: &[u8]) -> io::Result<()> {
    write_frame(writer, KIND_INPUT, &[&session.0, bytes])
}

pub fn decode_input(body: &[u8]) -> Result<(SessionId, &[u8]), FrameError> {
    split_session(body)
}

fn split_session(body: &[u8]) -> Result<(SessionId, &[u8]), FrameError> {
    match body.split_first_chunk::<SESSION_ID_LEN>() {
        Some((id, rest)) => Ok((SessionId(*id), rest)),
        None => Err(FrameError::Malformed(
            "data frame shorter than a session id".into(),
        )),
    }
}

/// First frame in each direction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    #[serde(default)]
    pub contract: u32,
    #[serde(default)]
    pub min_contract: u32,
    /// Release version of the sender. For people to read; never compared.
    #[serde(default)]
    pub version: String,
    /// Process id of the host. Absent in the client's hello.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

impl Hello {
    /// The hello this build sends as a client.
    pub fn local(version: impl Into<String>) -> Self {
        Self {
            contract: CONTRACT,
            min_contract: MIN_CONTRACT,
            version: version.into(),
            pid: None,
        }
    }
}

/// Which side of a handshake cannot follow the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incompatible {
    /// The peer speaks a contract older than we accept.
    PeerTooOld,
    /// We speak a contract older than the peer accepts.
    LocalTooOld,
}

/// Decide whether two sides can talk, and at which contract.
///
/// Only the two numbers decide. Comparing release versions instead would
/// refuse every window that was updated while its sessions kept running,
/// which is the one case the host exists for.
pub fn negotiate(local: &Hello, peer: &Hello) -> Result<u32, Incompatible> {
    if peer.contract < local.min_contract {
        return Err(Incompatible::PeerTooOld);
    }
    if local.contract < peer.min_contract {
        return Err(Incompatible::LocalTooOld);
    }
    Ok(local.contract.min(peer.contract))
}

/// Body of a [`KIND_ERROR`] frame and of a failed reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

impl WireError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// A request. `op` is kept as a string and `args` as raw JSON so that a host
/// which does not know an `op` can still read the `id` and answer it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    pub op: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplyEnvelope {
    pub id: u64,
    #[serde(default)]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
    #[serde(default)]
    pub result: Value,
}

/// What to start and how.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// `argv[0]` is the program.
    #[serde(default)]
    pub argv: Vec<String>,
    /// The complete environment of the child. The host adds nothing of its
    /// own to it: the host may have been started by an older release, or
    /// from another terminal, and its environment says nothing about the
    /// pane being opened.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
    #[serde(default)]
    pub label: Option<String>,
    /// Not read by the host; returned by `list` as given. It is where a
    /// window records which of its panes the session belongs to, so a
    /// window that starts later can find its sessions again.
    #[serde(default)]
    pub exported: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpawnArgs {
    #[serde(default)]
    pub spec: SpawnSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResult {
    pub session: SessionId,
    #[serde(default)]
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArgs {
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachArgs {
    pub session: SessionId,
    /// Sequence number to start at. Absent means the start of the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seq: Option<u64>,
}

/// Answer to `attach`. It is written before the first stream frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachResult {
    pub session: SessionId,
    /// Sequence number the stream begins at.
    #[serde(default)]
    pub start_seq: u64,
    /// True when the requested point had already left the log, so the
    /// stream begins later than asked.
    #[serde(default)]
    pub gap: bool,
    /// Terminal size in effect at `start_seq`: what an emulator replaying
    /// the stream must start from.
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeArgs {
    pub session: SessionId,
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShutdownArgs {
    #[serde(default)]
    pub force: bool,
}

/// How a session's child ended. Exactly one of the two is set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitStatus {
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
    #[serde(default)]
    pub cwd: String,
    /// `argv[0]` of the spawn spec.
    #[serde(default)]
    pub program: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Milliseconds since the Unix epoch.
    #[serde(default)]
    pub created_at_ms: u64,
    /// Absent while the child runs.
    #[serde(default)]
    pub exited: Option<ExitStatus>,
    /// Oldest sequence number still in the log.
    #[serde(default)]
    pub log_head_seq: u64,
    /// Sequence number the next byte or record will get.
    #[serde(default)]
    pub log_tail_seq: u64,
    /// Bytes the log holds right now.
    #[serde(default)]
    pub log_bytes: u64,
    /// Streams currently attached, over all connections.
    #[serde(default)]
    pub attached: u32,
    #[serde(default)]
    pub exported: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListResult {
    #[serde(default)]
    pub sessions: Vec<SessionInfo>,
}

/// Body of a [`KIND_EVENT`] frame. One flat object for every event: the
/// fields an event does not use stay at their defaults, and an event name
/// the reader does not know is skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBody {
    pub event: String,
    pub session: SessionId,
    /// `resized`: position of the resize record in the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    /// `exited`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// `gap`: the first sequence number that was lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_from: Option<u64>,
    /// `gap`: the sequence number the stream goes on from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumes_at: Option<u64>,
}

impl EventBody {
    fn named(event: &str, session: SessionId) -> Self {
        Self {
            event: event.to_string(),
            session,
            seq: None,
            cols: None,
            rows: None,
            code: None,
            signal: None,
            lost_from: None,
            resumes_at: None,
        }
    }

    pub fn resized(session: SessionId, seq: u64, cols: u16, rows: u16) -> Self {
        Self {
            seq: Some(seq),
            cols: Some(cols),
            rows: Some(rows),
            ..Self::named(EVENT_RESIZED, session)
        }
    }

    pub fn exited(session: SessionId, status: ExitStatus) -> Self {
        Self {
            code: status.code,
            signal: status.signal,
            ..Self::named(EVENT_EXITED, session)
        }
    }

    pub fn gap(session: SessionId, lost_from: u64, resumes_at: u64) -> Self {
        Self {
            lost_from: Some(lost_from),
            resumes_at: Some(resumes_at),
            ..Self::named(EVENT_GAP, session)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(contract: u32, min_contract: u32) -> Hello {
        Hello {
            contract,
            min_contract,
            version: String::new(),
            pid: None,
        }
    }

    #[test]
    fn a_frame_survives_the_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, KIND_REQUEST, &[b"ab", b"", b"cd"]).unwrap();
        assert_eq!(&wire[..5], &[5, 0, 0, 0, KIND_REQUEST]);
        let frame = read_frame(&mut wire.as_slice()).unwrap();
        assert_eq!(frame.kind, KIND_REQUEST);
        assert_eq!(frame.body, b"abcd");
    }

    #[test]
    fn an_oversized_length_is_refused_before_the_body_is_read() {
        // Only the header is supplied: reaching for the body would be an
        // unexpected-EOF error instead of `Oversized`.
        let mut wire = Vec::new();
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        wire.push(KIND_OUTPUT);
        assert!(matches!(
            read_frame(&mut wire.as_slice()),
            Err(FrameError::Oversized(len)) if len == u32::MAX as usize
        ));
    }

    #[test]
    fn a_zero_length_and_a_clean_close_are_told_apart() {
        assert!(matches!(
            read_frame(&mut [0u8, 0, 0, 0, 1].as_slice()),
            Err(FrameError::Empty)
        ));
        assert!(matches!(
            read_frame(&mut [].as_slice()),
            Err(FrameError::Closed)
        ));
        assert!(matches!(
            read_frame(&mut [3u8, 0].as_slice()),
            Err(FrameError::Io(_))
        ));
    }

    #[test]
    fn a_frame_too_large_to_send_is_not_sent() {
        let body = vec![0u8; MAX_FRAME_LEN];
        let mut wire = Vec::new();
        assert!(write_frame(&mut wire, KIND_OUTPUT, &[&body]).is_err());
        assert!(wire.is_empty());
    }

    /// A reader that times out between every byte it yields.
    struct Dribble<'a> {
        data: &'a [u8],
        ready: bool,
    }

    impl Read for Dribble<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.ready = !self.ready;
            if !self.ready {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let Some((first, rest)) = self.data.split_first() else {
                return Ok(0);
            };
            buf[0] = *first;
            self.data = rest;
            Ok(1)
        }
    }

    #[test]
    fn timeouts_inside_a_frame_lose_nothing() {
        let mut wire = Vec::new();
        write_frame(&mut wire, KIND_INPUT, &[b"hello"]).unwrap();
        let mut reader = Dribble {
            data: &wire,
            ready: true,
        };
        let frame = read_frame_patiently(&mut reader, || true).unwrap();
        assert_eq!(frame.body, b"hello");

        let mut reader = Dribble {
            data: &wire,
            ready: true,
        };
        assert!(matches!(
            read_frame_patiently(&mut reader, || false),
            Err(FrameError::Stopped)
        ));
    }

    #[test]
    fn data_frames_round_trip() {
        let session = SessionId::new_random();
        let mut wire = Vec::new();
        write_output(&mut wire, session, 0x0102_0304_0506_0708, b"out").unwrap();
        let frame = read_frame(&mut wire.as_slice()).unwrap();
        assert_eq!(frame.kind, KIND_OUTPUT);
        assert_eq!(
            decode_output(&frame.body).unwrap(),
            OutputFrame {
                session,
                seq: 0x0102_0304_0506_0708,
                bytes: b"out"
            }
        );

        let mut wire = Vec::new();
        write_input(&mut wire, session, b"in").unwrap();
        let frame = read_frame(&mut wire.as_slice()).unwrap();
        assert_eq!(decode_input(&frame.body).unwrap(), (session, &b"in"[..]));

        assert!(decode_input(&[0u8; 15]).is_err());
        assert!(decode_output(&[0u8; 23]).is_err());
    }

    #[test]
    fn compatibility_is_decided_by_the_contract_numbers_alone() {
        assert_eq!(negotiate(&hello(1, 1), &hello(1, 1)), Ok(1));
        // A newer peer that still accepts us: talk at the older contract.
        assert_eq!(negotiate(&hello(1, 1), &hello(4, 1)), Ok(1));
        assert_eq!(negotiate(&hello(4, 1), &hello(2, 2)), Ok(2));
        assert_eq!(
            negotiate(&hello(3, 2), &hello(1, 1)),
            Err(Incompatible::PeerTooOld)
        );
        assert_eq!(
            negotiate(&hello(1, 1), &hello(3, 2)),
            Err(Incompatible::LocalTooOld)
        );
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_default() {
        let hello: Hello =
            decode_json(br#"{"contract":7,"min_contract":1,"later":{"x":1}}"#).unwrap();
        assert_eq!(hello.contract, 7);
        assert_eq!(hello.version, "");

        let request: RequestEnvelope =
            decode_json(br#"{"id":3,"op":"from_the_future","extra":true}"#).unwrap();
        assert_eq!(request.op, "from_the_future");
        assert!(request.args.is_null());

        let spec: SpawnSpec = decode_json(br#"{"argv":["sh"],"colour":"blue"}"#).unwrap();
        assert_eq!(spec.argv, ["sh"]);
        assert!(spec.exported.is_empty());

        let id = SessionId::new_random();
        let info: SessionInfo = decode_json(format!(r#"{{"id":"{id}"}}"#).as_bytes()).unwrap();
        assert_eq!(info.id, id);
        assert!(info.exited.is_none());
    }

    #[test]
    fn a_session_id_is_text_in_json_and_sixteen_bytes_in_a_data_frame() {
        let id = SessionId([7; 16]);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"07070707-0707-0707-0707-070707070707\"");
        assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<SessionId>("\"nope\"").is_err());
    }
}
