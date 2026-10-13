//! A pane whose terminal runs in the session host.
//!
//! The shell of such a pane is started by the host, not by the window. Its
//! output arrives over the host's socket instead of from a PTY the window
//! owns, and what the window has to say - input, a new size - goes back the
//! same way. The emulator, the view and the renderer are the ones every
//! other pane uses; only the two ends of the byte stream differ.
//!
//! This file is the window's side of that connection. It imports nothing of
//! the terminal emulator: what touches an emulator is behind [`StreamSink`],
//! and a terminal that can be given one is an [`AttachTarget`].
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
//!
//! # Who ends a session
//!
//! A session belongs to the host, and the host's own watcher is what ends
//! its processes if the host dies. The window starts no watcher for it and
//! does not end it by going away: a window that quits, is updated or
//! crashes leaves its sessions running, and the next window finds them.
//! Two things end a session from here. Closing its pane does, and so does
//! leaving the app with the sessions to be stopped, which asks the host to
//! end everything it holds.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use splitlane_host::protocol::ERR_LIVE_SESSIONS;
use splitlane_host::protocol::{ERR_NO_SESSION, ERR_SESSION_EXITED, ERR_SESSION_LIVE};
use splitlane_host::{
    AttachFrom, Attachment, Client, Facts, HOST_SUBCOMMAND, HostError, SessionId, SessionInfo,
    SpawnSpec, StreamEvent,
};

use super::held_sessions::HeldSession;
use super::pty_session::{MAX_PENDING_INPUT_BYTES, SpawnParams, end_process_group};
use super::stream_replay::{AttachMode, SinkStep, StreamStart};

/// What a pane writes into its terminal when the connection to the host
/// is lost, before it shows as ended.
pub(super) const CONNECTION_LOST_NOTICE: &[u8] =
    b"\r\n\x1b[0;2mThe connection to the session host was lost.\x1b[0m\r\n";

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

/// How long a host of another release that holds nothing is given to go
/// away before a host of this release is started in its place.
const RETIRE_WAIT: Duration = Duration::from_secs(3);

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

    /// Bytes of input queued and not yet taken by the sending thread.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(super) fn queued_input_bytes(&self) -> usize {
        self.queued_input.load(Ordering::Acquire)
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
                Some(Launcher {
                    exe: launcher,
                    args: vec![HOST_SUBCOMMAND.to_string()],
                }),
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
    /// What to start as a host when none answers. `None` only connects,
    /// for a host somebody else runs.
    launcher: Option<Launcher>,
    /// The release this window is, to tell a host of another one by.
    release: String,
    state: Mutex<EndpointState>,
    /// Set once the window is going away and its sessions are to stay.
    /// From then on a pane that is dropped lets go of its session without
    /// ending it.
    leaving: AtomicBool,
    /// The sessions this window's panes follow, with the pid of each one's
    /// child: what is ended by hand when the host cannot be asked to.
    followed: Mutex<BTreeMap<SessionId, u32>>,
}

/// A command that runs as a session host.
pub(super) struct Launcher {
    pub(super) exe: PathBuf,
    pub(super) args: Vec<String>,
}

impl Launcher {
    fn start(&self, socket: &std::path::Path) -> Result<Client, HostError> {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        splitlane_host::ensure_running(&self.exe, &args, socket)
    }
}

#[derive(Default)]
struct EndpointState {
    control: Option<Arc<Client>>,
    /// When opening a session in the host last failed.
    failed_at: Option<Instant>,
}

impl HostEndpoint {
    pub(super) fn new(socket: PathBuf, launcher: Option<Launcher>) -> Arc<Self> {
        Self::of_release(socket, launcher, env!("CARGO_PKG_VERSION"))
    }

    /// An endpoint of a window that is the release `release`.
    pub(super) fn of_release(
        socket: PathBuf,
        launcher: Option<Launcher>,
        release: &str,
    ) -> Arc<Self> {
        Arc::new(Self {
            socket,
            launcher,
            release: release.to_string(),
            state: Mutex::new(EndpointState::default()),
            leaving: AtomicBool::new(false),
            followed: Mutex::new(BTreeMap::new()),
        })
    }

    /// The window is going away and what runs in the host stays. Every way
    /// out of the process that the toolkit knows of comes here before the
    /// panes are torn down; a process that is killed outright never tears
    /// them down.
    pub(super) fn leave_sessions_running(&self) {
        self.leaving.store(true, Ordering::Release);
    }

    pub(super) fn leaving(&self) -> bool {
        self.leaving.load(Ordering::Acquire)
    }

    /// Let a host of another release go once it holds nothing, and start
    /// one of this release in its place. Returns the connection to use.
    ///
    /// A host outlives the window that started it, so after an update the
    /// new window is served by the old release's host. That is wanted for
    /// as long as the host holds a session, and for no longer: with nothing
    /// to lose, the code that runs should be the code that is installed.
    /// The host itself decides - it refuses to stop while a session runs -
    /// so a session started by somebody else in the meantime is safe.
    fn retire_other_release(&self, control: Client) -> Result<Client, HostError> {
        let Some(launcher) = &self.launcher else {
            return Ok(control);
        };
        if control.host().version == self.release {
            return Ok(control);
        }
        match control.shutdown(false) {
            Ok(()) => {}
            Err(e) if e.remote_code() == Some(ERR_LIVE_SESSIONS) => return Ok(control),
            Err(e) => {
                log::warn!(
                    target: "splitlane::terminal::host",
                    "the session host of release {} would not make way: {e}",
                    control.host().version
                );
                return Ok(control);
            }
        }
        log::info!(
            target: "splitlane::terminal::host",
            "the session host of release {} held nothing and was stopped; starting {}",
            control.host().version,
            self.release
        );
        drop(control);
        let until = Instant::now() + RETIRE_WAIT;
        loop {
            match Client::connect(&self.socket) {
                Err(HostError::NotRunning) => break,
                _ if Instant::now() >= until => break,
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        launcher.start(&self.socket)
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
            // A host of another release is asked again each time, for it
            // may have emptied since.
            .filter(|control| !control.is_closed() && control.host().version == self.release)
        {
            return Ok(control.clone());
        }
        // Let go of first: the host counts it as somebody connected.
        state.control = None;
        let connected = match &self.launcher {
            Some(launcher) => launcher.start(&self.socket),
            None => Client::connect(&self.socket),
        };
        match connected.and_then(|control| self.retire_other_release(control)) {
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
        self.existing_session().ok()
    }

    /// [`Self::for_existing_session`], saying why there is no connection:
    /// nothing listening and a host that would not answer are different
    /// news about a session.
    fn existing_session(&self) -> Result<Arc<Client>, HostError> {
        let mut state = lock(&self.state);
        if let Some(control) = state
            .control
            .as_ref()
            .filter(|control| !control.is_closed())
        {
            return Ok(control.clone());
        }
        let control = Arc::new(Client::connect(&self.socket)?);
        state.control = Some(control.clone());
        Ok(control)
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

/// Where a pane's reader puts the stream of its session: an emulator, and
/// whatever else reads the output on its way into one.
///
/// Everything here is called on the reader's thread and nowhere else.
pub(super) trait StreamSink: Send + 'static {
    /// Take one event of the stream, in the order the host sent it.
    fn handle(&mut self, event: StreamEvent) -> SinkStep;

    /// When the sink has to be told the stream is quiet although the
    /// reader would wait longer. `None` while it holds nothing back.
    fn sync_deadline(&self) -> Option<Instant>;

    /// No event came for as long as the reader waited.
    fn quiet(&mut self);

    /// The connection to the host is gone while the session was running.
    /// Nothing follows.
    fn connection_lost(&mut self);
}

/// A terminal that can follow a session in the host.
pub(super) trait AttachTarget {
    type Sink: StreamSink;
    /// What the sink reports besides the grid, for the terminal state that
    /// owns the other half.
    type Channels;

    /// A sink that feeds this terminal a stream beginning at `start`.
    fn sink(&self, mode: AttachMode, start: StreamStart) -> (Self::Sink, Self::Channels);

    /// What this kind of terminal starts a shell with when the window
    /// starts it: the host is asked for exactly that.
    fn shell_start(params: &SpawnParams) -> anyhow::Result<ShellStart>;
}

/// The environment of a pane's shell, complete, and the directory it
/// starts in.
///
/// A shell the window starts inherits the window's environment, with what
/// the terminal's own spawn adds to it and takes from it, and the pane's
/// variables on top. A shell the host starts inherits nothing: the host
/// may have been started by another release or from a terminal, and its
/// environment says nothing about this pane. So the same environment is
/// put together in the window and sent whole. The directory is settled
/// here too, because each spawn has its own idea of where a shell starts
/// when the pane's directory is not there, and the host refuses one that
/// is not.
pub(super) struct ShellStart {
    pub(super) env: Vec<(String, String)>,
    pub(super) cwd: PathBuf,
}

/// What a terminal state needs to become a pane of a session in the host.
/// `C` is what its sink reports: [`AttachTarget::Channels`].
pub(super) struct SpawnedRemote<C> {
    pub(super) sender: RemoteSender,
    pub(super) child_pid: u32,
    pub(super) cwd: PathBuf,
    pub(super) channels: C,
    pub(super) handle: RemoteHandle,
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
pub(super) fn open_remote<T: AttachTarget>(
    params: &SpawnParams,
    pending: &T,
    endpoint: &Arc<HostEndpoint>,
    facts: Facts,
    surface: u64,
) -> Result<SpawnedRemote<T::Channels>, OpenError> {
    let control = endpoint.for_new_session().map_err(OpenError::BeforeSpawn)?;
    // The pane's own connection before the shell, not after: a host that
    // has no room for one more connection then refuses while there is
    // still nothing to lose.
    let pane = Client::connect(&endpoint.socket).map_err(|e| {
        endpoint.failed();
        OpenError::BeforeSpawn(anyhow!(e).context("cannot open the pane's connection"))
    })?;
    start_and_follow(params, pending, endpoint, &control, pane, facts, surface)
}

/// The rest of [`open_remote`], with both connections in hand: start the
/// shell over `control` and follow it over `pane`.
pub(super) fn start_and_follow<T: AttachTarget>(
    params: &SpawnParams,
    pending: &T,
    endpoint: &Arc<HostEndpoint>,
    control: &Client,
    pane: Client,
    facts: Facts,
    surface: u64,
) -> Result<SpawnedRemote<T::Channels>, OpenError> {
    // The id the shell is told is its pane's, which the view made up and
    // recorded. Recorded again here for whoever opens a pane without one.
    crate::surface_alias::remember(params.surface_id, surface);
    let started = spawn_spec::<T>(params, facts).and_then(|spec| {
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
    // No watcher is started here. The host has one for every session,
    // which follows the host, and a session is meant to outlive this
    // window.
    match follow(
        endpoint,
        pane,
        Followed {
            session,
            pid,
            surface,
        },
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
        }),
        Err(e) => {
            crate::surface_alias::forget(surface);
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

/// Why a terminal could not be attached to a session it was saved with.
#[derive(Debug)]
pub(super) enum ReattachError {
    /// The session is not there to attach to: no host answers, the host
    /// does not know it, or its child has ended. The terminal was not
    /// touched and can be given a new shell.
    Gone(anyhow::Error),
    /// The session is there and the terminal could not follow it.
    Failed(anyhow::Error),
    /// A host is there and did not say whether the session is, or said it
    /// is and would not take the pane's connection. The session may be
    /// running, with an agent in it: a new shell that resumed the same
    /// conversation would put a second agent beside the first.
    Unanswered(anyhow::Error),
}

/// Why a session a pane was recorded with may still be running, for a pane
/// that cannot be attached to it; `None` when it is gone.
///
/// A pane that runs on another terminal than the one the host answers with
/// stays in the window. If it was saved with a session, that session is
/// not its to follow any more, and a new shell typed the command that
/// resumes the same conversation would put a second agent beside the one
/// still working in the host. So such a pane asks first, by the rule
/// [`reattach`] goes by: nothing listening is an answer, and so is a host
/// that says the session is not there or has ended; anything else is not.
#[cfg(not(target_os = "macos"))]
pub(super) fn may_still_run(
    endpoint: &Arc<HostEndpoint>,
    session: SessionId,
) -> Option<anyhow::Error> {
    let control = match endpoint.existing_session() {
        Ok(control) => control,
        Err(HostError::NotRunning) => return None,
        Err(other) => {
            return Some(
                anyhow!(other).context("the session host could not be asked about the session"),
            );
        }
    };
    match control.list() {
        Ok(held) => held
            .into_iter()
            .find(|info| info.id == session && info.exited.is_none())
            .map(|_| anyhow!("session {session} is running in the session host")),
        Err(error) => Some(anyhow!(error).context("the session host did not say what it holds")),
    }
}

/// Attach a terminal to a session that is already running in the host.
/// The terminal first replays what the session has printed. `surface` is
/// the pane the terminal is shown in.
///
/// The session's program was told its pane's id when it started, under a
/// window that may be gone; what it and its hooks say from now on carries
/// that id, and it is recorded here as meaning `surface`.
pub(super) fn reattach<T: AttachTarget>(
    endpoint: &Arc<HostEndpoint>,
    session: SessionId,
    pending: &T,
    facts: Facts,
    surface: u64,
) -> Result<SpawnedRemote<T::Channels>, ReattachError> {
    let gone = ReattachError::Gone;
    // Nothing listening is an answer: a host takes its sessions with it.
    // Anything else a host says or fails to say is not.
    let control = endpoint.existing_session().map_err(|e| match e {
        HostError::NotRunning => gone(anyhow!(
            "no session host answers on {}",
            endpoint.socket.display()
        )),
        other => ReattachError::Unanswered(
            anyhow!(other).context("the session host could not be asked about the session"),
        ),
    })?;
    let info = control
        .list()
        .map_err(|e| {
            ReattachError::Unanswered(
                anyhow!(e).context("the session host did not say what it holds"),
            )
        })?
        .into_iter()
        .find(|info| info.id == session)
        .ok_or_else(|| gone(anyhow!("the session host has no session {session}")))?;
    if info.exited.is_some() {
        // Nobody will read its log now; a session that ended while no
        // window was there is only held until somebody says so.
        let _ = control.forget(session);
        return Err(gone(anyhow!("session {session} has ended")));
    }
    let pane = Client::connect(&endpoint.socket).map_err(|e| {
        ReattachError::Unanswered(anyhow!(e).context("cannot open the pane's connection"))
    })?;
    let exported = exported_id(&info, EXPORTED_SURFACE);
    if let Some(exported) = exported {
        crate::surface_alias::remember(exported, surface);
    }
    let followed = Followed {
        session,
        pid: info.pid,
        surface,
    };
    let (sender, channels, handle) = follow(
        endpoint,
        pane,
        followed,
        pending,
        AttachMode::Reattach,
        None,
    )
    .map_err(|e| {
        crate::surface_alias::forget(surface);
        ReattachError::Failed(e)
    })?;
    // The session has the colors of the window it was started under, or of
    // none.
    sender.send(RemoteMsg::Facts(Box::new(facts)));
    Ok(SpawnedRemote {
        sender,
        child_pid: info.pid,
        cwd: PathBuf::from(info.cwd),
        channels,
        handle,
    })
}

/// Keys of what a window records about a pane when it starts the pane's
/// session, for the window that finds the session later.
const EXPORTED_SURFACE: &str = "surface_id";
const EXPORTED_WORKSPACE: &str = "workspace_id";

fn exported_id(info: &SessionInfo, key: &str) -> Option<u64> {
    info.exported.get(key)?.parse().ok()
}

/// A session a pane follows.
#[derive(Clone, Copy)]
struct Followed {
    session: SessionId,
    /// The session's child.
    pid: u32,
    /// The pane.
    surface: u64,
}

/// Attach to `session` over `pane`, a connection for this pane alone, and
/// start the pane's two threads on it. `size` is the size the session's
/// PTY is known to have, if it is.
fn follow<T: AttachTarget>(
    endpoint: &Arc<HostEndpoint>,
    pane: Client,
    followed: Followed,
    pending: &T,
    mode: AttachMode,
    size: Option<(u16, u16)>,
) -> anyhow::Result<(RemoteSender, T::Channels, RemoteHandle)> {
    let Followed { session, pid, .. } = followed;
    let pane = Arc::new(pane);
    let attachment = pane
        .attach(session, AttachFrom::Start)
        .context("cannot attach to the session")?;
    let (sink, channels) = pending.sink(
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
        followed,
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
    lock(&endpoint.followed).insert(session, pid);
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
struct PaneReader<S> {
    endpoint: Arc<HostEndpoint>,
    followed: Followed,
    attachment: Attachment,
    sink: S,
    to_writer: RemoteSender,
    closing: Arc<AtomicBool>,
    _pane: Arc<Client>,
}

impl<S: StreamSink> PaneReader<S> {
    fn run(mut self) {
        // Set once the pane was closed: from then on the stream is only
        // emptied, until the session's end comes through it or the wait
        // is over.
        let mut ending: Option<Instant> = None;
        let Followed {
            session,
            pid,
            surface,
        } = self.followed;
        // The pane was dropped because the window is going away, and the
        // session stays.
        let mut left = false;
        let exited = loop {
            if ending.is_none() && self.closing.load(Ordering::Acquire) {
                if self.endpoint.leaving() {
                    left = true;
                    break false;
                }
                let asked = self
                    .endpoint
                    .for_existing_session()
                    .is_some_and(|control| control.kill(session).is_ok());
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
                            "session {session}: the connection to the session host was lost"
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
            attachment,
            ..
        } = self;
        // Let go of the stream before the session is forgotten, so the
        // host is not left writing to it.
        drop(attachment);
        lock(&endpoint.followed).remove(&session);
        crate::surface_alias::forget(surface);
        // A window that goes away between the two checks still ends
        // nothing: a session that has not ended is not ended from here
        // once the window is leaving.
        if left || (!exited && endpoint.leaving()) {
            return;
        }
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

/// A session as the rest of the app is shown it.
fn held(info: &SessionInfo) -> HeldSession {
    HeldSession {
        id: info.id.to_string(),
        pid: info.pid,
        cwd: info.cwd.clone(),
        program: info.program.clone(),
        created_at_ms: info.created_at_ms,
        exported_surface: exported_id(info, EXPORTED_SURFACE),
        exported_workspace: exported_id(info, EXPORTED_WORKSPACE),
    }
}

impl HostEndpoint {
    /// The sessions the host holds whose child is running. Empty when no
    /// host answers; never starts one.
    ///
    /// Sessions whose child ended while no window was there are forgotten
    /// on the way: their logs are held for a reader, and none will come.
    pub(super) fn held_sessions(&self) -> Vec<HeldSession> {
        let Some(control) = self.for_existing_session() else {
            return Vec::new();
        };
        let listed = match control.list() {
            Ok(listed) => listed,
            Err(e) => {
                log::warn!(
                    target: "splitlane::terminal::host",
                    "cannot list the sessions the session host holds: {e}"
                );
                return Vec::new();
            }
        };
        let followed = lock(&self.followed).clone();
        let mut held = Vec::new();
        for info in &listed {
            if info.exited.is_none() {
                held.push(self::held(info));
            } else if !followed.contains_key(&info.id) {
                let _ = control.forget(info.id);
            }
        }
        held
    }

    /// End a session nobody shows, and have the host forget it. `pid` is
    /// its shell's, for a host that can no longer be asked.
    pub(super) fn end_held(&self, session: SessionId, pid: u32) {
        end_session(self, session, pid, false);
    }

    /// Ask the host to end every session and exit. True when it said it
    /// would, or when no host is there to hold anything.
    fn stop_host(&self) -> bool {
        match self.for_existing_session() {
            Some(control) => control.shutdown(true).is_ok(),
            None => true,
        }
    }
}

/// End everything the host of this build holds, because the app is being
/// left with its sessions to be stopped. Waits up to `wait` for the host
/// to take the request; a host that does not is not waited for, and the
/// sessions this window shows are then ended from here, as they are for a
/// PTY the window owns.
pub(super) fn stop_all(endpoint: &Arc<HostEndpoint>, wait: Duration) {
    let (done_tx, done_rx) = mpsc::channel();
    let asked = std::thread::Builder::new()
        .name("pane-host-stop".into())
        .spawn({
            let endpoint = endpoint.clone();
            move || {
                let _ = done_tx.send(endpoint.stop_host());
            }
        });
    let stopped = asked.is_ok() && done_rx.recv_timeout(wait).unwrap_or(false);
    // Whoever ends them, the panes about to be dropped must not each ask
    // again.
    endpoint.leave_sessions_running();
    if !stopped {
        log::warn!(
            target: "splitlane::terminal::host",
            "the session host did not take the request to stop; ending this window's sessions directly"
        );
        // Side by side: each takes the tenth of a second it gives a
        // process group to go by itself.
        let pids: Vec<u32> = lock(&endpoint.followed).values().copied().collect();
        let enders: Vec<_> = pids
            .into_iter()
            .filter_map(|pid| {
                std::thread::Builder::new()
                    .spawn(move || end_process_group(pid))
                    .ok()
            })
            .collect();
        for ender in enders {
            let _ = ender.join();
        }
    }
}

/// How a pane ended up with a shell. `C` is what the pane's sink reports
/// and `W` what opening the pane in the window gives.
pub(super) enum Opened<C, W> {
    InHost(SpawnedRemote<C>),
    /// The host was not used, or could not be: the pane has a PTY the
    /// window owns, as every pane has without the host.
    InWindow(W),
    /// The host started the shell and the pane lost it. An error for the
    /// pane to show.
    Lost(anyhow::Error),
}

/// Open a pane's shell in the host, or in the window if the host cannot
/// take it. `endpoint` is `None` when this build has nowhere to look for a
/// host. `in_window` is how the terminal is given a PTY the window owns.
pub(super) fn open_or<T: AttachTarget, W>(
    endpoint: Option<&Arc<HostEndpoint>>,
    facts: Facts,
    params: SpawnParams,
    pending: T,
    surface: u64,
    in_window: impl FnOnce(SpawnParams, T) -> W,
) -> Opened<T::Channels, W> {
    let refused = match endpoint {
        Some(endpoint) => match open_remote(&params, &pending, endpoint, facts, surface) {
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
    Opened::InWindow(in_window(params, pending))
}

/// [`open_or`] for the terminal the window draws with on macOS, as the
/// tests of that terminal open a pane.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn open(
    endpoint: Option<&Arc<HostEndpoint>>,
    facts: Facts,
    params: SpawnParams,
    pending: super::pty_session::PendingTerminalBackend,
    signal_mask: Option<super::pty_session::ForegroundSignalMask>,
    surface: u64,
) -> Opened<super::pty_session::RemoteSinkChannels, anyhow::Result<super::pty_session::SpawnedPty>>
{
    open_or(
        endpoint,
        facts,
        params,
        pending,
        surface,
        |params, pending| {
            super::pty_session::TerminalState::open_pty_and_eventloop(params, pending, signal_mask)
        },
    )
}

/// How a pane that is shown for the first time came by its shell.
pub(super) struct Settled<C, W> {
    pub(super) opened: Opened<C, W>,
    /// The pane was saved with a session and is attached to it, or would
    /// have been: it is not a new pane, whatever else happened.
    pub(super) attached_to_held: bool,
    /// The host did not say whether the session the pane was saved with is
    /// still there. The pane keeps being recorded with it.
    pub(super) held_unanswered: bool,
}

/// Give a pane its shell when it is first shown, in a window whose panes
/// run in the host: the session it was saved with if that is still
/// running, and a new one otherwise - in the host, or in the window when
/// the host cannot take it.
///
/// A session the host did not answer for is not known to be gone, so it
/// is not replaced: the pane says so and starts nothing.
pub(super) fn attach_or_open<T: AttachTarget, W>(
    endpoint: Option<&Arc<HostEndpoint>>,
    held: Option<SessionId>,
    facts: Facts,
    params: SpawnParams,
    pending: T,
    surface: u64,
    in_window: impl FnOnce(SpawnParams, T) -> W,
) -> Settled<T::Channels, W> {
    let mut held_unanswered = false;
    let attached = held.zip(endpoint).and_then(|(session, endpoint)| {
        match reattach(endpoint, session, &pending, facts.clone(), surface) {
            Ok(spawned) => {
                log::info!(
                    target: "splitlane::terminal::host",
                    "session {session}: pane attached to it again, pid {}",
                    spawned.child_pid
                );
                Some(Opened::InHost(spawned))
            }
            Err(ReattachError::Gone(error)) => {
                log::info!(
                    target: "splitlane::terminal::host",
                    "session {session} is not there to attach to, opening a new shell: {error:#}"
                );
                None
            }
            Err(ReattachError::Failed(error)) => Some(Opened::Lost(error)),
            Err(ReattachError::Unanswered(error)) => {
                held_unanswered = true;
                Some(Opened::Lost(error.context(
                    "this pane's session may still be running in the session host, so it was not started again; restart Splitlane to attach to it",
                )))
            }
        }
    });
    match attached {
        Some(opened) => Settled {
            opened,
            attached_to_held: true,
            held_unanswered,
        },
        None => Settled {
            opened: open_or(endpoint, facts, params, pending, surface, in_window),
            attached_to_held: false,
            held_unanswered,
        },
    }
}

/// What the host is asked to start for a pane: the shell the window would
/// have started, where it would have started it, with the environment it
/// would have had.
fn spawn_spec<T: AttachTarget>(params: &SpawnParams, facts: Facts) -> anyhow::Result<SpawnSpec> {
    let mut argv = vec![params.shell.clone()];
    argv.extend(params.extra_args.iter().cloned());
    let ShellStart { env, cwd } = T::shell_start(params)?;
    let cwd = cwd
        .into_os_string()
        .into_string()
        .map_err(|cwd| anyhow!("the working directory {cwd:?} is not UTF-8"))?;
    let mut exported = BTreeMap::new();
    exported.insert(EXPORTED_SURFACE.to_string(), params.surface_id.to_string());
    if let Some(workspace_id) = params.env.get("SPLITLANE_WORKSPACE_ID") {
        exported.insert(EXPORTED_WORKSPACE.to_string(), workspace_id.clone());
    }
    exported.insert("window_pid".to_string(), std::process::id().to_string());
    Ok(SpawnSpec {
        argv,
        env,
        cwd,
        cols: u16::try_from(params.cols).unwrap_or(u16::MAX),
        rows: u16::try_from(params.rows).unwrap_or(u16::MAX),
        label: None,
        exported,
        facts: Some(facts),
    })
}

/// How the terminal the window draws with on macOS starts a shell.
#[cfg(target_os = "macos")]
pub(super) fn alacritty_shell_start(params: &SpawnParams) -> anyhow::Result<ShellStart> {
    // The window's own spawn ignores a directory that is not there and
    // starts the shell where the window is.
    let cwd = if params.cwd.is_dir() {
        params.cwd.clone()
    } else {
        crate::launch_cwd::implicit_launch_cwd()
    };
    Ok(ShellStart {
        env: child_environment(std::env::vars_os(), &params.env, params.surface_id)?,
        cwd,
    })
}

/// The environment of a shell alacritty's spawn starts: `inherited`, the
/// window's own, with what that spawn adds to it and takes from it, and
/// the pane's variables.
#[cfg(target_os = "macos")]
pub(super) fn child_environment(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    pane: &std::collections::HashMap<String, String>,
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
#[cfg(target_os = "macos")]
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
pub(super) fn password_entry() -> Option<(String, String)> {
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
