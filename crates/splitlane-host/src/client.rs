//! A blocking client for the session host.
//!
//! One connection carries requests, their replies and any number of
//! attached streams. A reader thread sorts what arrives: a reply goes to
//! the call that is waiting for it, a stream frame to the [`Attachment`] of
//! its session.
//!
//! Stream frames are handed over through a bounded queue. A consumer that
//! stops taking events therefore stops the reader, the socket fills, and the
//! host sees a client that fell behind and treats it as one: nothing is
//! buffered without bound on either side, and when the consumer comes back
//! it gets a [`StreamEvent::Gap`] if the log moved on meanwhile.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{RecvHalf, SendHalf, Stream};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::lock;
use crate::protocol::{
    self, AttachArgs, AttachResult, EventBody, ExitStatus, FrameError, Hello, ListResult,
    ReplyEnvelope, RequestEnvelope, ResizeArgs, SessionArgs, SessionId, SessionInfo, ShutdownArgs,
    SpawnArgs, SpawnResult, SpawnSpec, WireError,
};
use crate::HostError;

/// How long a connect waits for the host's side of the handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a request waits for its reply.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long [`ensure_running`] waits for a host it started to answer.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);

/// Receive timeout on the socket: how often the reader thread looks up to
/// see whether the client was dropped.
const READ_TICK: Duration = Duration::from_millis(100);
/// Stream events that may wait for the consumer of one attachment.
const STREAM_QUEUE: usize = 256;

/// Where an attached stream starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachFrom {
    /// The beginning of the session. If the log no longer reaches that far
    /// back the stream starts at its head and the attachment reports a gap.
    Start,
    /// A sequence number, normally the one a previous stream stopped at.
    Seq(u64),
}

/// One thing that happened in an attached session, in log order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Output. `seq` is the sequence number of the first byte.
    Output { seq: u64, bytes: Vec<u8> },
    /// The terminal was resized at this position of the log.
    Resized { seq: u64, cols: u16, rows: u16 },
    /// The log moved past this stream. Everything from `lost_from` up to
    /// `resumes_at` is gone and the stream goes on from `resumes_at`.
    Gap { lost_from: u64, resumes_at: u64 },
    /// The child ended and all of its output was delivered. Last event.
    Exited(ExitStatus),
}

/// An attached stream. Iterating blocks for the next event and ends after
/// [`StreamEvent::Exited`], after a detach, or when the connection is lost.
/// Dropping it detaches the stream.
pub struct Attachment {
    pub session: SessionId,
    /// Sequence number the stream begins at.
    pub start_seq: u64,
    /// True when the stream begins later than was asked for.
    pub gap: bool,
    /// Terminal size in effect at `start_seq`.
    pub cols: u16,
    pub rows: u16,
    events: Receiver<StreamEvent>,
    /// Id of the request that opened the stream; tells this attachment's
    /// slot from a later one for the same session.
    opened_by: u64,
    client: Weak<Inner>,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("session", &self.session)
            .field("start_seq", &self.start_seq)
            .field("gap", &self.gap)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let Some(inner) = self.client.upgrade() else {
            return;
        };
        // Only if the slot is still this attachment's: after a detach or an
        // exit it is gone, and the session may have been attached again.
        let slot = {
            let mut streams = lock(&inner.streams);
            match streams.get(&self.session) {
                Some(slot) if slot.opened_by == self.opened_by => streams.remove(&self.session),
                _ => None,
            }
        };
        if let Some(slot) = slot {
            slot.cancelled.store(true, Ordering::SeqCst);
            // Without this the host would go on streaming to nobody, and
            // the next attach to the session would be refused as a second
            // one. Nobody waits for the answer.
            let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
            let _ = inner.send_request(
                id,
                protocol::OP_DETACH,
                &SessionArgs {
                    session: self.session,
                },
            );
        }
    }
}

impl Attachment {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<StreamEvent, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }
}

impl Iterator for Attachment {
    type Item = StreamEvent;

    fn next(&mut self) -> Option<StreamEvent> {
        self.events.recv().ok()
    }
}

struct StreamSlot {
    events: SyncSender<StreamEvent>,
    /// Set by `detach`, so a reader waiting on a full queue lets go.
    cancelled: Arc<AtomicBool>,
    /// Id of the `attach` request this slot was made for.
    opened_by: u64,
    /// False until the reply to that request has been read. Frames of an
    /// earlier stream of the same session can still be on their way when
    /// the slot is registered; the host writes all of them before the
    /// reply, so whatever arrives ahead of it belongs to the old stream.
    live: bool,
}

struct Inner {
    writer: Mutex<SendHalf>,
    pending: Mutex<HashMap<u64, mpsc::Sender<ReplyEnvelope>>>,
    streams: Mutex<HashMap<SessionId, StreamSlot>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl Inner {
    fn send_request<T: Serialize>(&self, id: u64, op: &str, args: &T) -> Result<(), HostError> {
        let envelope = RequestEnvelope {
            id,
            op: op.to_string(),
            args: serde_json::to_value(args).map_err(|e| HostError::Protocol(e.to_string()))?,
        };
        let mut writer = lock(&self.writer);
        protocol::write_json(&mut *writer, protocol::KIND_REQUEST, &envelope)?;
        Ok(())
    }
}

/// A connection to a host. Dropping it closes the connection, which
/// detaches its streams and leaves the sessions running.
pub struct Client {
    inner: Arc<Inner>,
    host: Hello,
    contract: u32,
}

impl Client {
    /// Connect to the host on `socket_path` and shake hands.
    ///
    /// [`HostError::NotRunning`] when nothing listens there, and
    /// [`HostError::Incompatible`] when a host answers that this build
    /// cannot talk to.
    pub fn connect(socket_path: &Path) -> Result<Client, HostError> {
        Self::connect_with(
            socket_path,
            &Hello::local(env!("CARGO_PKG_VERSION")),
            HANDSHAKE_TIMEOUT,
        )
    }

    /// [`Client::connect`] with the hello and the handshake deadline chosen
    /// by the caller.
    pub fn connect_with(
        socket_path: &Path,
        hello: &Hello,
        timeout: Duration,
    ) -> Result<Client, HostError> {
        let stream = open(socket_path)?;
        stream.set_recv_timeout(Some(READ_TICK))?;
        let (mut recv, mut send) = stream.split();
        protocol::write_json(&mut send, protocol::KIND_HELLO, hello)?;

        let deadline = Instant::now() + timeout;
        let frame = protocol::read_frame_patiently(&mut recv, || Instant::now() < deadline)
            .map_err(|e| match e {
                FrameError::Stopped => HostError::Timeout("waiting for the host's hello".into()),
                FrameError::Closed => HostError::Protocol(
                    "the host closed the connection during the handshake".into(),
                ),
                other => HostError::Protocol(other.to_string()),
            })?;
        let host: Hello = match frame.kind {
            protocol::KIND_HELLO => protocol::decode_json(&frame.body)
                .map_err(|e| HostError::Protocol(e.to_string()))?,
            protocol::KIND_ERROR => {
                let error: WireError = protocol::decode_json(&frame.body)
                    .map_err(|e| HostError::Protocol(e.to_string()))?;
                return Err(if error.code == protocol::ERR_INCOMPATIBLE {
                    HostError::Incompatible(error.message)
                } else {
                    HostError::Remote {
                        code: error.code,
                        message: error.message,
                    }
                });
            }
            kind => {
                return Err(HostError::Protocol(format!(
                    "the host opened with frame kind {kind} instead of a hello"
                )))
            }
        };
        // The host checks too, but a host older than this check is exactly
        // the one that might not.
        let contract = protocol::negotiate(hello, &host).map_err(|side| {
            HostError::Incompatible(match side {
                protocol::Incompatible::PeerTooOld => format!(
                    "the session host is too old: it speaks contract {}, this client needs at least {}",
                    host.contract, hello.min_contract
                ),
                protocol::Incompatible::LocalTooOld => format!(
                    "this client is too old: it speaks contract {}, the session host needs at least {}",
                    hello.contract, host.min_contract
                ),
            })
        })?;

        let inner = Arc::new(Inner {
            writer: Mutex::new(send),
            pending: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });
        let reader = inner.clone();
        std::thread::Builder::new()
            .name("host-client-reader".into())
            .spawn(move || read_loop(recv, reader))?;
        Ok(Client {
            inner,
            host,
            contract,
        })
    }

    /// What the host said about itself in the handshake.
    pub fn host(&self) -> &Hello {
        &self.host
    }

    /// The contract both sides agreed on.
    pub fn contract(&self) -> u32 {
        self.contract
    }

    fn request<A: Serialize, R: DeserializeOwned>(
        &self,
        op: &str,
        args: &A,
    ) -> Result<R, HostError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.request_as(id, op, args)
    }

    fn request_as<A: Serialize, R: DeserializeOwned>(
        &self,
        id: u64,
        op: &str,
        args: &A,
    ) -> Result<R, HostError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        lock(&self.inner.pending).insert(id, reply_tx);
        let outcome = self.inner.send_request(id, op, args).and_then(|()| {
            match reply_rx.recv_timeout(REQUEST_TIMEOUT) {
                Ok(reply) => Ok(reply),
                Err(RecvTimeoutError::Timeout) => Err(HostError::Timeout(format!(
                    "waiting for the reply to `{op}`"
                ))),
                Err(RecvTimeoutError::Disconnected) => Err(HostError::Io(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the connection to the session host was lost",
                ))),
            }
        });
        lock(&self.inner.pending).remove(&id);
        let reply = outcome?;
        if !reply.ok {
            let error = reply.error.unwrap_or_default();
            return Err(HostError::Remote {
                code: error.code,
                message: error.message,
            });
        }
        // A reply with nothing in it is `null`; read it as an empty object
        // so a result type made only of optional fields accepts it.
        let result = match reply.result {
            Value::Null => Value::Object(Default::default()),
            other => other,
        };
        serde_json::from_value(result).map_err(|e| HostError::Protocol(e.to_string()))
    }

    /// Start a session. Returns its id and the pid of its child.
    pub fn spawn(&self, spec: SpawnSpec) -> Result<(SessionId, u32), HostError> {
        let result: SpawnResult = self.request(protocol::OP_SPAWN, &SpawnArgs { spec })?;
        Ok((result.session, result.pid))
    }

    pub fn list(&self) -> Result<Vec<SessionInfo>, HostError> {
        let result: ListResult = self.request(protocol::OP_LIST, &Value::Null)?;
        Ok(result.sessions)
    }

    /// Follow a session's output from `from`. One attachment per session on
    /// a connection.
    pub fn attach(&self, session: SessionId, from: AttachFrom) -> Result<Attachment, HostError> {
        let (events_tx, events_rx) = mpsc::sync_channel(STREAM_QUEUE);
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        {
            // Registered before the request goes out: the first stream
            // frame can follow the reply immediately.
            let mut streams = lock(&self.inner.streams);
            if streams.contains_key(&session) {
                return Err(HostError::Remote {
                    code: protocol::ERR_ALREADY_ATTACHED.into(),
                    message: "this connection already follows the session".into(),
                });
            }
            streams.insert(
                session,
                StreamSlot {
                    events: events_tx,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    opened_by: id,
                    live: false,
                },
            );
        }
        let args = AttachArgs {
            session,
            from_seq: match from {
                AttachFrom::Start => None,
                AttachFrom::Seq(seq) => Some(seq),
            },
        };
        match self.request_as::<_, AttachResult>(id, protocol::OP_ATTACH, &args) {
            Ok(result) => Ok(Attachment {
                session,
                start_seq: result.start_seq,
                gap: result.gap,
                cols: result.cols,
                rows: result.rows,
                events: events_rx,
                opened_by: id,
                client: Arc::downgrade(&self.inner),
            }),
            Err(e) => {
                let mut streams = lock(&self.inner.streams);
                if streams
                    .get(&session)
                    .is_some_and(|slot| slot.opened_by == id)
                {
                    streams.remove(&session);
                }
                Err(e)
            }
        }
    }

    /// Stop following a session. Events already queued stay readable from
    /// the attachment; then it ends.
    pub fn detach(&self, session: SessionId) -> Result<(), HostError> {
        if let Some(slot) = lock(&self.inner.streams).remove(&session) {
            slot.cancelled.store(true, Ordering::SeqCst);
        }
        self.request::<_, Value>(protocol::OP_DETACH, &SessionArgs { session })
            .map(drop)
    }

    /// Send input to a session. There is no reply: input for a session that
    /// has ended is dropped by the host.
    pub fn input(&self, session: SessionId, bytes: &[u8]) -> Result<(), HostError> {
        for chunk in bytes.chunks(protocol::MAX_DATA_CHUNK) {
            let mut writer = lock(&self.inner.writer);
            protocol::write_input(&mut *writer, session, chunk)?;
        }
        Ok(())
    }

    pub fn resize(&self, session: SessionId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.request::<_, Value>(
            protocol::OP_RESIZE,
            &ResizeArgs {
                session,
                cols,
                rows,
            },
        )
        .map(drop)
    }

    /// Terminate every process of the session. It stays listed, as exited,
    /// until it is forgotten.
    pub fn kill(&self, session: SessionId) -> Result<(), HostError> {
        self.request::<_, Value>(protocol::OP_KILL, &SessionArgs { session })
            .map(drop)
    }

    /// Drop an exited session and its log.
    pub fn forget(&self, session: SessionId) -> Result<(), HostError> {
        self.request::<_, Value>(protocol::OP_FORGET, &SessionArgs { session })
            .map(drop)
    }

    /// Ask the host to exit. Without `force` it refuses while a session is
    /// still running; with it, the running sessions are terminated first.
    pub fn shutdown(&self, force: bool) -> Result<(), HostError> {
        self.request::<_, Value>(protocol::OP_SHUTDOWN, &ShutdownArgs { force })
            .map(drop)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The reader thread holds the receiving half and sees this within
        // one tick; the socket closes when it lets go.
        self.inner.closed.store(true, Ordering::SeqCst);
    }
}

fn read_loop(mut recv: RecvHalf, inner: Arc<Inner>) {
    // Ends on a closed or broken connection, and when the client is dropped.
    while let Ok(frame) =
        protocol::read_frame_patiently(&mut recv, || !inner.closed.load(Ordering::SeqCst))
    {
        match frame.kind {
            protocol::KIND_REPLY => {
                let Ok(reply) = protocol::decode_json::<ReplyEnvelope>(&frame.body) else {
                    break;
                };
                if reply.ok {
                    if let Some(slot) = lock(&inner.streams)
                        .values_mut()
                        .find(|slot| slot.opened_by == reply.id)
                    {
                        slot.live = true;
                    }
                }
                if let Some(waiter) = lock(&inner.pending).remove(&reply.id) {
                    let _ = waiter.send(reply);
                }
            }
            protocol::KIND_OUTPUT => {
                let Ok(output) = protocol::decode_output(&frame.body) else {
                    break;
                };
                deliver(
                    &inner,
                    output.session,
                    StreamEvent::Output {
                        seq: output.seq,
                        bytes: output.bytes.to_vec(),
                    },
                );
            }
            protocol::KIND_EVENT => {
                let Ok(event) = protocol::decode_json::<EventBody>(&frame.body) else {
                    break;
                };
                // An event name from a newer host is not ours to interpret.
                if let Some(stream_event) = stream_event(&event) {
                    let last = matches!(stream_event, StreamEvent::Exited(_));
                    deliver(&inner, event.session, stream_event);
                    if last {
                        lock(&inner.streams).remove(&event.session);
                    }
                }
            }
            // The host gave up on the connection and said why; the read
            // that follows finds it closed.
            protocol::KIND_ERROR => {}
            // A frame kind from a newer host.
            _ => {}
        }
    }
    inner.closed.store(true, Ordering::SeqCst);
    // Dropping the senders is what ends every waiting call and every
    // attachment.
    lock(&inner.pending).clear();
    lock(&inner.streams).clear();
}

fn stream_event(event: &EventBody) -> Option<StreamEvent> {
    match event.event.as_str() {
        protocol::EVENT_RESIZED => Some(StreamEvent::Resized {
            seq: event.seq?,
            cols: event.cols?,
            rows: event.rows?,
        }),
        protocol::EVENT_GAP => Some(StreamEvent::Gap {
            lost_from: event.lost_from?,
            resumes_at: event.resumes_at?,
        }),
        protocol::EVENT_EXITED => Some(StreamEvent::Exited(ExitStatus {
            code: event.code,
            signal: event.signal,
        })),
        _ => None,
    }
}

/// Hand an event to the attachment of `session`, waiting while its queue is
/// full. The wait ends when the consumer takes an event, when the stream is
/// detached or dropped, or when the client is closed.
fn deliver(inner: &Inner, session: SessionId, mut event: StreamEvent) {
    let (events, cancelled) = {
        let streams = lock(&inner.streams);
        let Some(slot) = streams.get(&session) else {
            // Detached a moment ago; the host had this frame on its way.
            return;
        };
        if !slot.live {
            // Left over from an earlier stream of this session.
            return;
        }
        (slot.events.clone(), slot.cancelled.clone())
    };
    loop {
        match events.try_send(event) {
            Ok(()) => return,
            Err(TrySendError::Full(back)) => {
                if cancelled.load(Ordering::SeqCst) || inner.closed.load(Ordering::SeqCst) {
                    return;
                }
                event = back;
                std::thread::sleep(Duration::from_millis(1));
            }
            // The attachment is being dropped, and its drop is what tells
            // the host. Touching the slot here could remove one that a new
            // attach has registered since.
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

#[cfg(unix)]
fn open(socket_path: &Path) -> Result<Stream, HostError> {
    use interprocess::local_socket::GenericFilePath;

    let name = socket_path.to_fs_name::<GenericFilePath>()?;
    let stream = Stream::connect(name).map_err(|e| match e.kind() {
        // No socket file, or a socket file nobody listens on any more.
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => HostError::NotRunning,
        _ => HostError::Io(e),
    })?;
    // Everything typed into a pane goes to whoever listens here, so the
    // listener has to be ours. The socket's directory normally guarantees
    // that; a path the caller chose in a shared directory does not.
    // SAFETY: geteuid has no preconditions and cannot fail.
    let ours = unsafe { libc::geteuid() };
    if let Ok(Some(theirs)) = stream.peer_creds().map(|creds| creds.euid()) {
        if theirs != ours {
            return Err(HostError::Config(format!(
                "the session host on {} belongs to another user",
                socket_path.display()
            )));
        }
    }
    Ok(stream)
}

#[cfg(not(unix))]
fn open(_socket_path: &Path) -> Result<Stream, HostError> {
    Err(HostError::Unsupported)
}

/// Connect to the host on `socket_path`, starting `exe args...` as the host
/// first if nothing answers there.
///
/// The host is started in a session of its own with no standard streams, so
/// it outlives the caller. Several callers may race: each may start a host,
/// one host takes the socket, the others see that it is taken and exit, and
/// every caller ends up connected to the one that stayed.
pub fn ensure_running(exe: &Path, args: &[&str], socket_path: &Path) -> Result<Client, HostError> {
    match Client::connect(socket_path) {
        Err(HostError::NotRunning) => {}
        other => return other,
    }
    spawn_detached(exe, args)?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match Client::connect(socket_path) {
            Ok(client) => return Ok(client),
            Err(e @ (HostError::Incompatible(_) | HostError::Unsupported)) => return Err(e),
            Err(e) if Instant::now() >= deadline => {
                return Err(HostError::Timeout(format!(
                    "waiting for the session host to start ({e})"
                )))
            }
            // Not bound yet, or bound and not accepting yet.
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[cfg(unix)]
fn spawn_detached(exe: &Path, args: &[&str]) -> Result<(), HostError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let descriptor_limit = crate::descriptors::limit();
    // SAFETY: `setsid` and `fcntl` are async-signal-safe and are the only
    // calls made between fork and exec.
    unsafe {
        command.pre_exec(move || {
            crate::descriptors::seal_inherited(descriptor_limit);
            // Out of the caller's session and process group: closing the
            // terminal the caller runs in, or signalling its group, must
            // not reach the host.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // The host is meant to outlive us, so nobody waits for it in the usual
    // sense; this thread only collects it if it exits first, as a host that
    // lost the race for the socket does at once.
    std::thread::Builder::new()
        .name("host-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

#[cfg(not(unix))]
fn spawn_detached(_exe: &Path, _args: &[&str]) -> Result<(), HostError> {
    Err(HostError::Unsupported)
}
