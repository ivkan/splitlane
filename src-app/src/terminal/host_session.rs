//! A pane whose terminal runs in the session host.
//!
//! The shell of such a pane is started by the host, not by the window. Its
//! output arrives over the host's socket instead of from a PTY the window
//! owns, and what the window has to say - input, a new size - goes back the
//! same way. The emulator, the view and the renderer are the ones every
//! other pane uses; only the two ends of the byte stream differ.
//!
//! This file is the window's side of that connection. It imports nothing of
//! the terminal emulator: what touches the emulator is in `pty_session`.
//!
//! # Connections and threads
//!
//! A pane has a connection of its own for its stream, its input and its
//! resizes, and two threads on it. The *reader* takes the stream's events
//! and feeds them to the terminal. The *writer* sends what the pane has to
//! say. They are two because of how the host's client works: the reply to
//! a request is delivered by the same socket reader that hands out stream
//! events, and that reader stops while the stream's queue is full. A
//! thread that both consumed the stream and waited for a reply could wait
//! for itself.
//!
//! Starting, ending and forgetting sessions goes over one more connection,
//! shared by all panes of the process. A pane's own connection can be held
//! up by its program - input a program does not read is not taken off the
//! socket by the host - and a pane that is being closed must still be able
//! to end its session.
//!
//! Neither kind of connection is ever touched by the thread that draws the
//! window. That thread puts messages on a queue and sets flags, and
//! dropping a connection, which can wait behind a stuck write, is left to
//! the pane's reader.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use splitlane_host::protocol::{ERR_NO_SESSION, ERR_SESSION_EXITED, ERR_SESSION_LIVE};
use splitlane_host::{
    AttachFrom, Attachment, Client, Facts, HOST_SUBCOMMAND, HostError, SessionId, SpawnSpec,
    StreamEvent,
};

use super::pty_session::{
    AttachMode, ForegroundSignalMask, MAX_PENDING_INPUT_BYTES, PendingTerminalBackend, RemoteSink,
    RemoteSinkChannels, SinkStep, SpawnParams, SpawnedPty, StreamStart, TerminalState,
    end_process_group,
};

/// How long a host that could not be reached, or would not start a session,
/// is left alone. Restoring a window opens its panes one after the other,
/// and each of them would otherwise wait out the same failure.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// How long a pane that is being closed waits for its session to end after
/// asking the host to end it.
const END_WAIT: Duration = Duration::from_secs(3);
/// How often the reader looks up from the stream to see whether its pane
/// was closed.
const STREAM_TICK: Duration = Duration::from_millis(100);

/// What a pane has to tell its session in the host.
///
/// Queued, and sent by a thread of the pane's own. The thread that calls
/// is the one that draws the window, and a request to the host waits for
/// its reply.
pub(super) enum RemoteMsg {
    Input(Cow<'static, [u8]>),
    /// The size of the grid, and of the whole text area in pixels. Zero
    /// pixels when the size of a cell is not known yet.
    Resize {
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    /// What the window knows about itself and the program may ask for.
    Facts(Box<Facts>),
    /// Ask the program to repaint, after output was lost on the way here.
    Redraw,
    /// The pane is gone. Nothing queued behind this is sent.
    Shutdown,
}

/// The sending end of a pane's queue to the host.
#[derive(Clone)]
pub(super) struct RemoteSender {
    queue: Sender<RemoteMsg>,
    /// Bytes of input queued and not yet taken by the sending thread.
    queued_input: Arc<AtomicUsize>,
    input_limit: usize,
}

/// The receiving end, held by the thread that talks to the host.
pub(super) struct RemoteReceiver {
    queue: Receiver<RemoteMsg>,
    queued_input: Arc<AtomicUsize>,
}

/// A queue from a pane to its session. `input_limit` bounds the input that
/// may wait in it.
///
/// The queue itself has no bound, because a resize or a shutdown must never
/// be refused. Input is the only thing that can pile up: a program that
/// stops reading holds up the host's end of the connection, and everything
/// typed or pasted after that would otherwise be kept here for ever.
pub(super) fn remote_queue(input_limit: usize) -> (RemoteSender, RemoteReceiver) {
    let (queue_tx, queue_rx) = mpsc::channel();
    let queued_input = Arc::new(AtomicUsize::new(0));
    (
        RemoteSender {
            queue: queue_tx,
            queued_input: queued_input.clone(),
            input_limit,
        },
        RemoteReceiver {
            queue: queue_rx,
            queued_input,
        },
    )
}

impl RemoteSender {
    /// Queue input. False when it was refused because too much is waiting
    /// already; the caller counts that, as it does for input refused before
    /// the pane had a shell at all.
    pub(super) fn input(&self, bytes: Cow<'static, [u8]>) -> bool {
        let len = bytes.len();
        let reserved =
            self.queued_input
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                    queued
                        .checked_add(len)
                        .filter(|total| *total <= self.input_limit)
                });
        if reserved.is_err() {
            return false;
        }
        if self.queue.send(RemoteMsg::Input(bytes)).is_err() {
            // The sending thread is gone, which means the session is.
            // Input for it is dropped, as the host drops it.
            self.queued_input.fetch_sub(len, Ordering::AcqRel);
        }
        true
    }

    /// Queue anything that is not input. Never refused.
    pub(super) fn send(&self, msg: RemoteMsg) {
        let _ = self.queue.send(msg);
    }
}

impl RemoteReceiver {
    /// Wait for the next message. `None` once every sender is gone.
    pub(super) fn recv(&self) -> Option<RemoteMsg> {
        self.queue.recv().ok().map(|msg| self.taken(msg))
    }

    /// The next message if one is waiting.
    pub(super) fn try_recv(&self) -> Option<RemoteMsg> {
        match self.queue.try_recv() {
            Ok(msg) => Some(self.taken(msg)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    fn taken(&self, msg: RemoteMsg) -> RemoteMsg {
        if let RemoteMsg::Input(bytes) = &msg {
            self.queued_input.fetch_sub(bytes.len(), Ordering::AcqRel);
        }
        msg
    }
}

/// Whether new panes are opened in the session host.
///
/// Off unless the window was started with `SPLITLANE_SESSION_HOST=1`. An
/// environment variable and not a setting, on purpose: the path is new,
/// and a setting is a promise to keep. Read once, so a window is one or
/// the other for as long as it runs.
pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SPLITLANE_SESSION_HOST").is_some_and(|value| value == "1")
    })
}

/// The session host of this build: where it listens, and this binary to
/// start it with. `None` when there is no place for its socket.
pub(super) fn process_endpoint() -> Option<Arc<HostEndpoint>> {
    static ENDPOINT: OnceLock<Option<Arc<HostEndpoint>>> = OnceLock::new();
    ENDPOINT
        .get_or_init(|| {
            let socket = crate::runtime_paths::host_socket_path_spec()?;
            let launcher = std::env::current_exe().ok()?;
            Some(HostEndpoint::new(
                socket.path().to_path_buf(),
                Some(launcher),
            ))
        })
        .clone()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing guarded here is left half-changed by a panic.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Where the session host of this build listens, and the connection all
/// panes share for starting and ending sessions.
pub(super) struct HostEndpoint {
    socket: PathBuf,
    /// The binary to start as a host when none answers. `None` only
    /// connects, for a host somebody else runs.
    launcher: Option<PathBuf>,
    state: Mutex<EndpointState>,
}

#[derive(Default)]
struct EndpointState {
    control: Option<Arc<Client>>,
    /// When opening a session in the host last failed.
    failed_at: Option<Instant>,
}

impl HostEndpoint {
    pub(super) fn new(socket: PathBuf, launcher: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            socket,
            launcher,
            state: Mutex::new(EndpointState::default()),
        })
    }

    /// The shared connection, for starting a session. Starts a host if
    /// none answers.
    ///
    /// The lock is held while connecting, on purpose: panes that open at
    /// the same moment wait for the one attempt, and all but the first
    /// find either the connection or the note that it failed.
    fn for_new_session(&self) -> anyhow::Result<Arc<Client>> {
        let mut state = lock(&self.state);
        if let Some(failed_at) = state.failed_at {
            let ago = failed_at.elapsed();
            if ago < RETRY_AFTER {
                return Err(anyhow!(
                    "the session host failed {}s ago and is not tried again yet",
                    ago.as_secs()
                ));
            }
            state.failed_at = None;
        }
        if let Some(control) = state
            .control
            .as_ref()
            .filter(|control| !control.is_closed())
        {
            return Ok(control.clone());
        }
        let connected = match &self.launcher {
            Some(exe) => splitlane_host::ensure_running(exe, &[HOST_SUBCOMMAND], &self.socket),
            None => Client::connect(&self.socket),
        };
        match connected {
            Ok(control) => {
                let control = Arc::new(control);
                state.control = Some(control.clone());
                Ok(control)
            }
            Err(e) => {
                state.control = None;
                state.failed_at = Some(Instant::now());
                Err(anyhow!(e).context(format!(
                    "cannot reach the session host on {}",
                    self.socket.display()
                )))
            }
        }
    }

    /// The shared connection, for a session that exists. Never starts a
    /// host: one started now would not have the session.
    pub(super) fn for_existing_session(&self) -> Option<Arc<Client>> {
        let mut state = lock(&self.state);
        if let Some(control) = state
            .control
            .as_ref()
            .filter(|control| !control.is_closed())
        {
            return Some(control.clone());
        }
        let control = Arc::new(Client::connect(&self.socket).ok()?);
        state.control = Some(control.clone());
        Some(control)
    }

    /// Note that the host would not do what a new pane needs.
    fn failed(&self) {
        lock(&self.state).failed_at = Some(Instant::now());
    }

    /// Whether new panes are being kept away from the host for now.
    #[cfg(test)]
    pub(super) fn is_avoided(&self) -> bool {
        lock(&self.state)
            .failed_at
            .is_some_and(|failed_at| failed_at.elapsed() < RETRY_AFTER)
    }
}

/// A pane's hold on its session, kept by the terminal state.
pub(super) struct RemoteHandle {
    session: SessionId,
    closing: Arc<AtomicBool>,
    acked_size: Arc<AtomicU32>,
}

impl RemoteHandle {
    pub(super) fn session(&self) -> SessionId {
        self.session
    }

    /// The pane is gone: end the session. Returns at once; the pane's
    /// reader does the ending.
    pub(super) fn close(&self) {
        self.closing.store(true, Ordering::Release);
    }

    /// The grid size the host last confirmed it gave the PTY, as columns
    /// and rows. `None` until it has confirmed one.
    pub(super) fn acked_size(&self) -> Option<(usize, usize)> {
        unpack_size(self.acked_size.load(Ordering::Acquire))
    }
}

/// A handle that is let go of ends its session, closed or not. A pane that
/// is closed while its shell is still being started never gets as far as
/// owning the handle: what was started for it is dropped where the pane
/// should have been, and the shell would otherwise run on with nobody to
/// end it.
impl Drop for RemoteHandle {
    fn drop(&mut self) {
        self.close();
    }
}

fn pack_size(cols: u16, rows: u16) -> u32 {
    (u32::from(cols) << 16) | u32::from(rows)
}

/// Zero is "none": a PTY has no empty size.
fn unpack_size(packed: u32) -> Option<(usize, usize)> {
    (packed != 0).then_some(((packed >> 16) as usize, (packed & 0xffff) as usize))
}

/// What a terminal state needs to become a pane of a session in the host.
pub(super) struct SpawnedRemote {
    pub(super) sender: RemoteSender,
    pub(super) child_pid: u32,
    pub(super) cwd: PathBuf,
    pub(super) channels: RemoteSinkChannels,
    pub(super) handle: RemoteHandle,
    /// Ends the session's processes if the window dies without doing it.
    #[cfg(not(test))]
    pub(super) pty_guard: Option<crate::agents::parent_guard::PtyGuardHandle>,
}

/// Why a pane could not be opened in the host. Which of the two it is
/// decides what may be done instead.
pub(super) enum OpenError {
    /// The host started nothing. The pane can be opened as any other.
    BeforeSpawn(anyhow::Error),
    /// The host started the shell and the pane could not be connected to
    /// it. Starting another shell would be starting a second one.
    AfterSpawn(anyhow::Error),
}

/// Start a pane's shell in the host and attach the pane's terminal to it.
pub(super) fn open_remote(
    params: &SpawnParams,
    pending: &PendingTerminalBackend,
    endpoint: &Arc<HostEndpoint>,
    facts: Facts,
) -> Result<SpawnedRemote, OpenError> {
    let control = endpoint.for_new_session().map_err(OpenError::BeforeSpawn)?;
    // The pane's own connection before the shell, not after: a host that
    // has no room for one more connection then refuses while there is
    // still nothing to lose.
    let pane = Client::connect(&endpoint.socket).map_err(|e| {
        endpoint.failed();
        OpenError::BeforeSpawn(anyhow!(e).context("cannot open the pane's connection"))
    })?;
    start_and_follow(params, pending, endpoint, &control, pane, facts)
}

/// The rest of [`open_remote`], with both connections in hand: start the
/// shell over `control` and follow it over `pane`.
pub(super) fn start_and_follow(
    params: &SpawnParams,
    pending: &PendingTerminalBackend,
    endpoint: &Arc<HostEndpoint>,
    control: &Client,
    pane: Client,
    facts: Facts,
) -> Result<SpawnedRemote, OpenError> {
    let started = spawn_spec(params, facts).and_then(|spec| {
        let (cwd, size) = (PathBuf::from(&spec.cwd), (spec.cols, spec.rows));
        let (session, pid) = control
            .spawn(spec)
            .context("the session host did not start the shell")?;
        Ok((session, pid, cwd, size))
    });
    let (session, pid, cwd, size) = started.map_err(|e| {
        endpoint.failed();
        OpenError::BeforeSpawn(e)
    })?;
    // As for a PTY the window owns: if the window is killed outright, its
    // sessions go with it.
    #[cfg(not(test))]
    let pty_guard = crate::agents::parent_guard::spawn_pty_guard(pid);
    match follow(
        endpoint,
        pane,
        session,
        pid,
        pending,
        AttachMode::Fresh,
        Some(size),
    ) {
        Ok((sender, channels, handle)) => Ok(SpawnedRemote {
            sender,
            child_pid: pid,
            cwd,
            channels,
            handle,
            #[cfg(not(test))]
            pty_guard,
        }),
        Err(e) => {
            // Not waited for: ending a session takes as long as its child
            // takes to die, and the pane has an error to show now.
            let endpoint = endpoint.clone();
            let ended = std::thread::Builder::new()
                .name("pane-host-end".into())
                .spawn(move || end_session(&endpoint, session, pid, false));
            if ended.is_err() {
                end_process_group(pid);
            }
            Err(OpenError::AfterSpawn(e))
        }
    }
}

/// Attach a terminal to a session that is already running in the host.
/// The terminal first replays what the session has printed.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "a window does not look for its sessions yet")
)]
pub(super) fn reattach(
    endpoint: &Arc<HostEndpoint>,
    session: SessionId,
    pending: &PendingTerminalBackend,
) -> anyhow::Result<SpawnedRemote> {
    let control = endpoint
        .for_existing_session()
        .ok_or_else(|| anyhow!("no session host answers on {}", endpoint.socket.display()))?;
    let info = control
        .list()?
        .into_iter()
        .find(|info| info.id == session)
        .ok_or_else(|| anyhow!("the session host has no session {session}"))?;
    let pane = Client::connect(&endpoint.socket).context("cannot open the pane's connection")?;
    let (sender, channels, handle) = follow(
        endpoint,
        pane,
        session,
        info.pid,
        pending,
        AttachMode::Reattach,
        None,
    )?;
    Ok(SpawnedRemote {
        sender,
        child_pid: info.pid,
        cwd: PathBuf::from(info.cwd),
        channels,
        handle,
        #[cfg(not(test))]
        pty_guard: None,
    })
}

/// Attach to `session` over `pane`, a connection for this pane alone, and
/// start the pane's two threads on it. `size` is the size the session's
/// PTY is known to have, if it is.
fn follow(
    endpoint: &Arc<HostEndpoint>,
    pane: Client,
    session: SessionId,
    pid: u32,
    pending: &PendingTerminalBackend,
    mode: AttachMode,
    size: Option<(u16, u16)>,
) -> anyhow::Result<(RemoteSender, RemoteSinkChannels, RemoteHandle)> {
    let pane = Arc::new(pane);
    let attachment = pane
        .attach(session, AttachFrom::Start)
        .context("cannot attach to the session")?;
    let (sink, channels) = RemoteSink::new(
        pending,
        mode,
        StreamStart {
            start_seq: attachment.start_seq,
            tail_seq: attachment.tail_seq,
            gap: attachment.gap,
            cols: attachment.cols,
            rows: attachment.rows,
        },
    );
    let (sender, receiver) = remote_queue(MAX_PENDING_INPUT_BYTES);
    let closing = Arc::new(AtomicBool::new(false));
    let acked_size = Arc::new(AtomicU32::new(
        size.map_or(0, |(cols, rows)| pack_size(cols, rows)),
    ));

    std::thread::Builder::new()
        .name("pane-host-writer".into())
        .spawn({
            let (pane, acked_size) = (pane.clone(), acked_size.clone());
            move || write_requests(&pane, session, &receiver, &acked_size)
        })
        .context("cannot start the pane's writer thread")?;
    let reader = PaneReader {
        endpoint: endpoint.clone(),
        session,
        pid,
        attachment,
        sink,
        to_writer: sender.clone(),
        closing: closing.clone(),
        // Held for as long as the attachment is: the connection closes
        // when the last of the two threads lets go of it.
        _pane: pane,
    };
    let started = std::thread::Builder::new()
        .name("pane-host-reader".into())
        .spawn(move || reader.run());
    if let Err(e) = started {
        sender.send(RemoteMsg::Shutdown);
        return Err(anyhow!(e).context("cannot start the pane's reader thread"));
    }
    Ok((
        sender,
        channels,
        RemoteHandle {
            session,
            closing,
            acked_size,
        },
    ))
}

/// The thread that takes a pane's stream. Owns the attachment and the
/// sink, and is where both are dropped.
struct PaneReader {
    endpoint: Arc<HostEndpoint>,
    session: SessionId,
    /// The session's child, for ending its processes if the host cannot be
    /// asked to.
    pid: u32,
    attachment: Attachment,
    sink: RemoteSink,
    to_writer: RemoteSender,
    closing: Arc<AtomicBool>,
    _pane: Arc<Client>,
}

impl PaneReader {
    fn run(mut self) {
        // Set once the pane was closed: from then on the stream is only
        // emptied, until the session's end comes through it or the wait
        // is over.
        let mut ending: Option<Instant> = None;
        let exited = loop {
            if ending.is_none() && self.closing.load(Ordering::Acquire) {
                let asked = self
                    .endpoint
                    .for_existing_session()
                    .is_some_and(|control| control.kill(self.session).is_ok());
                if !asked {
                    // Nobody to wait for.
                    break false;
                }
                ending = Some(Instant::now() + END_WAIT);
            }
            if ending.is_some_and(|until| Instant::now() >= until) {
                break false;
            }
            let wait = self.sink.sync_deadline().map_or(STREAM_TICK, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(STREAM_TICK)
            });
            match self.attachment.recv_timeout(wait) {
                Ok(StreamEvent::Exited(status)) => {
                    if ending.is_none() {
                        self.sink.handle(StreamEvent::Exited(status));
                    }
                    break true;
                }
                // The pane is gone; nobody looks at its terminal.
                Ok(_) if ending.is_some() => {}
                Ok(event) => {
                    if self.sink.handle(event) == SinkStep::Redraw {
                        self.to_writer.send(RemoteMsg::Redraw);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if ending.is_none() {
                        self.sink.quiet();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if ending.is_none() {
                        log::warn!(
                            target: "splitlane::terminal::host",
                            "session {}: the connection to the session host was lost",
                            self.session
                        );
                        self.sink.connection_lost();
                    }
                    break false;
                }
            }
        };
        self.to_writer.send(RemoteMsg::Shutdown);
        let PaneReader {
            endpoint,
            session,
            pid,
            attachment,
            ..
        } = self;
        // Let go of the stream before the session is forgotten, so the
        // host is not left writing to it.
        drop(attachment);
        end_session(&endpoint, session, pid, exited);
    }
}

/// End a session and have the host forget it. `exited` when the host has
/// already said the child is gone. Waits for the child, so it is for a
/// thread that may.
///
/// If the host cannot be asked, the process group is ended from here, as
/// it is for a PTY the window owns. `pid` 0 is "not known" and leaves that
/// out.
fn end_session(endpoint: &HostEndpoint, session: SessionId, pid: u32, exited: bool) {
    let Some(control) = endpoint.for_existing_session() else {
        if !exited {
            end_process_group(pid);
        }
        return;
    };
    if !exited {
        let _ = control.kill(session);
    }
    // A session is forgotten only once its child was reaped and its last
    // output collected, which the host does a moment after the kill.
    let until = Instant::now() + END_WAIT;
    loop {
        match control.forget(session) {
            Ok(()) => return,
            Err(e) if e.remote_code() == Some(ERR_NO_SESSION) => return,
            Err(e) if e.remote_code() == Some(ERR_SESSION_LIVE) && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                log::warn!(
                    target: "splitlane::terminal::host",
                    "session {session}: the session host did not let go of it ({e}); ending its processes directly"
                );
                end_process_group(pid);
                return;
            }
        }
    }
}

/// Body of a pane's writer thread: send what the pane says, in order,
/// until the pane is gone.
fn write_requests(
    pane: &Client,
    session: SessionId,
    queue: &RemoteReceiver,
    acked_size: &AtomicU32,
) {
    // A message taken while looking for more of the same kind.
    let mut held: Option<RemoteMsg> = None;
    // The connection broke. The queue is still emptied, so that input does
    // not count against the limit for ever; nothing is sent.
    let mut broken = false;
    loop {
        let Some(msg) = held.take().or_else(|| queue.recv()) else {
            return;
        };
        let sent = match msg {
            RemoteMsg::Shutdown => return,
            _ if broken => Ok(()),
            RemoteMsg::Input(bytes) => pane.input(session, &bytes),
            RemoteMsg::Resize {
                mut cols,
                mut rows,
                mut pixel_width,
                mut pixel_height,
            } => {
                // Dragging a divider asks for a size per frame, and each
                // one sent is a signal to the program and a reply waited
                // for. Only the last of a run matters.
                loop {
                    match queue.try_recv() {
                        Some(RemoteMsg::Resize {
                            cols: next_cols,
                            rows: next_rows,
                            pixel_width: next_width,
                            pixel_height: next_height,
                        }) => {
                            (cols, rows) = (next_cols, next_rows);
                            (pixel_width, pixel_height) = (next_width, next_height);
                        }
                        other => {
                            held = other;
                            break;
                        }
                    }
                }
                let resized = if pixel_width == 0 || pixel_height == 0 {
                    pane.resize(session, cols, rows)
                } else {
                    pane.resize_with_pixels(session, cols, rows, pixel_width, pixel_height)
                };
                if resized.is_ok() {
                    acked_size.store(pack_size(cols, rows), Ordering::Release);
                }
                resized
            }
            RemoteMsg::Facts(facts) => pane.set_facts(session, &facts),
            RemoteMsg::Redraw => {
                // One repaint answers any number of requests for one.
                while let Some(next) = queue.try_recv() {
                    if !matches!(next, RemoteMsg::Redraw) {
                        held = Some(next);
                        break;
                    }
                }
                pane.redraw(session)
            }
        };
        match sent {
            Ok(()) => {}
            // The session ended between the pane saying this and the host
            // hearing it.
            Err(e) if e.remote_code() == Some(ERR_SESSION_EXITED) => {}
            Err(e) if e.remote_code() == Some(ERR_NO_SESSION) => {}
            Err(e @ (HostError::Io(_) | HostError::Protocol(_))) => {
                log::debug!(
                    target: "splitlane::terminal::host",
                    "session {session}: the pane's connection is gone ({e})"
                );
                broken = true;
            }
            Err(e) => log::warn!(
                target: "splitlane::terminal::host",
                "session {session}: the session host refused a request: {e}"
            ),
        }
    }
}

/// How a pane ended up with a shell.
pub(super) enum Opened {
    InHost(SpawnedRemote),
    /// The host was not used, or could not be: the pane has a PTY the
    /// window owns, as every pane has without the host.
    InWindow(anyhow::Result<SpawnedPty>),
    /// The host started the shell and the pane lost it. An error for the
    /// pane to show.
    Lost(anyhow::Error),
}

/// Open a pane's shell in the host, or in the window if the host cannot
/// take it. `endpoint` is `None` when this build has nowhere to look for a
/// host.
pub(super) fn open(
    endpoint: Option<&Arc<HostEndpoint>>,
    facts: Facts,
    params: SpawnParams,
    pending: PendingTerminalBackend,
    signal_mask: Option<ForegroundSignalMask>,
) -> Opened {
    let refused = match endpoint {
        Some(endpoint) => match open_remote(&params, &pending, endpoint, facts) {
            Ok(spawned) => {
                log::info!(
                    target: "splitlane::terminal::host",
                    "session {}: pane opened in the session host, pid {}",
                    spawned.handle.session(),
                    spawned.child_pid
                );
                return Opened::InHost(spawned);
            }
            Err(OpenError::AfterSpawn(e)) => return Opened::Lost(e),
            Err(OpenError::BeforeSpawn(e)) => e,
        },
        None => anyhow!("there is no place for a session host socket"),
    };
    log::warn!(
        target: "splitlane::terminal::host",
        "pane not opened in the session host, opening it in the window: {refused:#}"
    );
    Opened::InWindow(TerminalState::open_pty_and_eventloop(
        params,
        pending,
        signal_mask,
    ))
}

/// What the host is asked to start for a pane: the shell the window would
/// have started, where it would have started it, with the environment it
/// would have had.
fn spawn_spec(params: &SpawnParams, facts: Facts) -> anyhow::Result<SpawnSpec> {
    let mut argv = vec![params.shell.clone()];
    argv.extend(params.extra_args.iter().cloned());
    // The window's own spawn ignores a directory that is not there and
    // starts the shell where the window is. The host refuses one, so the
    // same choice is made before asking.
    let cwd = if params.cwd.is_dir() {
        params.cwd.clone()
    } else {
        crate::launch_cwd::implicit_launch_cwd()
    };
    let cwd = cwd
        .into_os_string()
        .into_string()
        .map_err(|cwd| anyhow!("the working directory {cwd:?} is not UTF-8"))?;
    let mut exported = BTreeMap::new();
    exported.insert("surface_id".to_string(), params.surface_id.to_string());
    if let Some(workspace_id) = params.env.get("SPLITLANE_WORKSPACE_ID") {
        exported.insert("workspace_id".to_string(), workspace_id.clone());
    }
    exported.insert("window_pid".to_string(), std::process::id().to_string());
    Ok(SpawnSpec {
        argv,
        env: child_environment(std::env::vars_os(), &params.env, params.surface_id)?,
        cwd,
        cols: u16::try_from(params.cols).unwrap_or(u16::MAX),
        rows: u16::try_from(params.rows).unwrap_or(u16::MAX),
        label: None,
        exported,
        facts: Some(facts),
    })
}

/// The environment of a pane's shell, complete.
///
/// A shell the window starts inherits the window's environment, with what
/// the terminal emulator's own spawn adds to it and takes from it, and the
/// pane's variables on top. A shell the host starts inherits nothing: the
/// host may have been started by another release or from a terminal, and
/// its environment says nothing about this pane. So the same environment
/// is put together here and sent whole. `inherited` is the window's.
pub(super) fn child_environment(
    inherited: impl Iterator<Item = (OsString, OsString)>,
    pane: &HashMap<String, String>,
    surface_id: u64,
) -> anyhow::Result<Vec<(String, String)>> {
    // A variable that is not UTF-8 cannot travel as text. It is left out.
    let mut env: BTreeMap<String, String> = inherited
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    // What alacritty's `tty::new` does to the child's environment, in its
    // order: the window id twice, the user and the home directory, then
    // the caller's variables, then two it removes whoever set them.
    let (user, home) = user_and_home(&env)?;
    let window_id = surface_id.to_string();
    env.insert("ALACRITTY_WINDOW_ID".into(), window_id.clone());
    env.insert("USER".into(), user);
    env.insert("HOME".into(), home);
    env.insert("WINDOWID".into(), window_id);
    env.extend(pane.iter().map(|(key, value)| (key.clone(), value.clone())));
    env.remove("XDG_ACTIVATION_TOKEN");
    env.remove("DESKTOP_STARTUP_ID");
    // What no process can be started with.
    env.retain(|key, value| !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'));
    Ok(env.into_iter().collect())
}

/// `USER` and `HOME` as alacritty's spawn sets them: from the environment,
/// and for a variable that is not there, from the password database.
fn user_and_home(env: &BTreeMap<String, String>) -> anyhow::Result<(String, String)> {
    if let (Some(user), Some(home)) = (env.get("USER"), env.get("HOME")) {
        return Ok((user.clone(), home.clone()));
    }
    let (user, home) = password_entry()
        .ok_or_else(|| anyhow!("the user has no entry in the password database"))?;
    Ok((
        env.get("USER").cloned().unwrap_or(user),
        env.get("HOME").cloned().unwrap_or(home),
    ))
}

/// Name and home directory of the user this process runs as.
fn password_entry() -> Option<(String, String)> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    // SAFETY: `passwd` is plain integers and pointers, so all-zero is a
    // valid value to be overwritten.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buffer` is as long
    // as it is said to be. The strings `entry` points to live in `buffer`.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() {
        return None;
    }
    let text = |pointer: *const libc::c_char| {
        if pointer.is_null() {
            return None;
        }
        // SAFETY: a non-null field of a found entry is a NUL-terminated
        // string inside `buffer`, which outlives this call.
        let text = unsafe { std::ffi::CStr::from_ptr(pointer) };
        text.to_str().ok().map(str::to_owned)
    };
    Some((text(entry.pw_name)?, text(entry.pw_dir)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_waits_up_to_the_limit_and_is_refused_beyond_it() {
        let (sender, receiver) = remote_queue(10);
        assert!(sender.input(b"12345678".as_slice().into()));
        // Would make twelve.
        assert!(!sender.input(b"abcd".as_slice().into()));
        assert!(sender.input(b"ab".as_slice().into()));
        assert!(!sender.input(b"c".as_slice().into()));

        // What the sending thread has taken no longer counts.
        assert!(matches!(
            receiver.recv(),
            Some(RemoteMsg::Input(bytes)) if bytes.as_ref() == b"12345678"
        ));
        assert!(sender.input(b"abcdefgh".as_slice().into()));
        assert!(matches!(
            receiver.try_recv(),
            Some(RemoteMsg::Input(bytes)) if bytes.as_ref() == b"ab"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Some(RemoteMsg::Input(bytes)) if bytes.as_ref() == b"abcdefgh"
        ));
        assert!(receiver.try_recv().is_none());
    }

    #[test]
    fn a_full_queue_still_takes_everything_that_is_not_input() {
        let (sender, receiver) = remote_queue(4);
        assert!(sender.input(b"full".as_slice().into()));
        assert!(!sender.input(b"x".as_slice().into()));
        sender.send(RemoteMsg::Resize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        });
        sender.send(RemoteMsg::Shutdown);
        assert!(matches!(receiver.recv(), Some(RemoteMsg::Input(_))));
        assert!(matches!(
            receiver.recv(),
            Some(RemoteMsg::Resize {
                cols: 80,
                rows: 24,
                ..
            })
        ));
        assert!(matches!(receiver.recv(), Some(RemoteMsg::Shutdown)));
    }

    #[test]
    fn input_for_a_session_that_is_gone_is_dropped_and_not_counted() {
        let (sender, receiver) = remote_queue(4);
        drop(receiver);
        // Accepted every time: nothing is waiting, because nothing can.
        for _ in 0..3 {
            assert!(sender.input(b"abcd".as_slice().into()));
        }
        sender.send(RemoteMsg::Shutdown);
    }
}
