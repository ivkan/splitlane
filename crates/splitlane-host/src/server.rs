//! The listener: who may connect, what a connection may ask, and when the
//! host exits.
//!
//! # Who may connect
//!
//! The socket is a file with mode 0600 in a directory with mode 0700, and
//! every accepted connection is also asked for its peer's user id. Anybody
//! who can connect can type into every session, so this is the same trust
//! boundary as the terminals themselves.
//!
//! # One host per socket
//!
//! A host holds an exclusive lock on `<socket>.lock` for as long as it runs.
//! The lock is what makes starting safe when several callers race: only its
//! holder may remove a socket file and bind a new one, so a socket file that
//! is found while holding the lock is stale by definition. A host that
//! cannot take the lock exits and leaves the running one alone.
//!
//! # When the host exits
//!
//! On a `shutdown` request, on SIGTERM or SIGINT if it was asked to handle
//! them, and after `idle_grace` with no running session and no connected
//! client. A session that has exited and was never forgotten does not count
//! as running: it holds a log somebody may still want to read, but nobody
//! is connected to read it, and a host that waited for a `forget` that never
//! comes would stay forever.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    GenericFilePath, Listener, ListenerNonblockingMode, ListenerOptions, SendHalf, Stream,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::client::Client;
use crate::log::Item;
use crate::protocol::{
    self, AttachArgs, AttachResult, EventBody, Frame, FrameError, Hello, Incompatible, ListResult,
    ReplyEnvelope, RequestEnvelope, ResizeArgs, SessionArgs, SessionId, ShutdownArgs, SpawnArgs,
    SpawnResult, WireError,
};
use crate::session::{LogBudget, Session, SessionSettings, KILL_GRACE};
use crate::{lock, HostConfig, HostError, MAX_SOCKET_PATH_BYTES};

/// How often the accept loop looks at everything that is not a connection:
/// the stop flag, the idle clock, the socket file.
const TICK_MS: i32 = 100;
/// Receive timeout on a connection: how often its thread checks whether the
/// host is stopping.
const READ_TICK: Duration = Duration::from_secs(1);
/// How long a new connection has to say hello.
const HELLO_DEADLINE: Duration = Duration::from_secs(5);
/// How long a host being started waits for an answer from whatever already
/// listens on its socket.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// How often the socket file is checked for having been removed or replaced.
const SOCKET_CHECK_EVERY: Duration = Duration::from_secs(2);
/// How long a stopping host waits for killed sessions to be reaped.
const REAP_DEADLINE: Duration = Duration::from_secs(2);

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    // Storing to an atomic is all a signal handler may safely do here.
    SIGNALLED.store(true, Ordering::SeqCst);
}

struct Host {
    hello: Hello,
    settings: SessionSettings,
    /// In the order they were started.
    sessions: Mutex<Vec<Arc<Session>>>,
    clients: AtomicUsize,
    max_connections: usize,
    /// Set when the host is going away, for whatever reason.
    stopping: AtomicBool,
    /// Set by a `shutdown` request that was accepted.
    shutdown_requested: AtomicBool,
}

impl Host {
    fn find(&self, id: SessionId) -> Option<Arc<Session>> {
        lock(&self.sessions)
            .iter()
            .find(|session| session.id == id)
            .cloned()
    }

    fn running(&self) -> Vec<Arc<Session>> {
        lock(&self.sessions)
            .iter()
            .filter(|session| session.exit_status().is_none())
            .cloned()
            .collect()
    }
}

pub(crate) fn run(config: HostConfig) -> Result<(), HostError> {
    let path = config.socket_path.clone();
    if path.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
        return Err(HostError::Config(format!(
            "socket path {} is longer than the {MAX_SOCKET_PATH_BYTES} bytes a Unix socket address holds",
            path.display()
        )));
    }
    prepare_socket_dir(&path, config.owns_socket_dir)?;
    let _lock = take_lock(&path)?;
    // The lock already says no host of this kind is running. This asks the
    // socket itself, for the case where the lock file was removed under a
    // running host and the lock just taken is on a new file.
    if Client::connect_with(&path, &Hello::local(config.version.clone()), PROBE_TIMEOUT).is_ok() {
        return Err(HostError::AlreadyRunning(path));
    }
    let (mut listener, mut inode) = bind(&path)?;

    if config.handle_signals {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: installs a handler that only stores to an atomic.
        unsafe {
            libc::signal(libc::SIGTERM, handler);
            libc::signal(libc::SIGINT, handler);
        }
    }

    let host = Arc::new(Host {
        hello: Hello {
            pid: Some(std::process::id()),
            ..Hello::local(config.version.clone())
        },
        settings: SessionSettings {
            log_capacity: config.log_capacity_bytes,
            budget: LogBudget::new(config.total_log_capacity_bytes),
            guard_exe: config.guard_exe.clone(),
        },
        sessions: Mutex::new(Vec::new()),
        clients: AtomicUsize::new(0),
        max_connections: config.max_connections,
        stopping: AtomicBool::new(false),
        shutdown_requested: AtomicBool::new(false),
    });
    log::info!(
        "host {} (pid {}) listening on {}",
        config.version,
        std::process::id(),
        path.display()
    );

    let mut idle_since: Option<Instant> = None;
    let mut socket_checked = Instant::now();
    let reason = loop {
        if host.shutdown_requested.load(Ordering::SeqCst) {
            break "shutdown requested";
        }
        if config.handle_signals && SIGNALLED.load(Ordering::SeqCst) {
            break "signal";
        }
        let busy = host.clients.load(Ordering::SeqCst) > 0 || !host.running().is_empty();
        if busy {
            idle_since = None;
        } else if idle_since.get_or_insert_with(Instant::now).elapsed() >= config.idle_grace {
            break "idle";
        }
        if socket_checked.elapsed() >= SOCKET_CHECK_EVERY {
            socket_checked = Instant::now();
            // Temporary directories get cleaned, and a host whose socket
            // file is gone holds sessions nobody can reach while its lock
            // keeps a replacement from starting.
            if socket_inode(&path) != Some(inode) {
                log::warn!(
                    "socket {} was removed or replaced; binding it again",
                    path.display()
                );
                match bind(&path) {
                    Ok((fresh, fresh_inode)) => {
                        listener = fresh;
                        inode = fresh_inode;
                    }
                    Err(e) => log::error!("cannot bind {} again: {e}", path.display()),
                }
            }
        }
        accept_pending(&listener, &host);
    };

    log::info!("host stopping: {reason}");
    host.stopping.store(true, Ordering::SeqCst);
    end_sessions(&host);
    if socket_inode(&path) == Some(inode) {
        let _ = std::fs::remove_file(&path);
    }
    drop(listener);
    log::info!("host stopped");
    Ok(())
}

/// Wait up to one tick for connections and start a thread for each.
fn accept_pending(listener: &Listener, host: &Arc<Host>) {
    let mut poll = libc::pollfd {
        fd: listener_fd(listener),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` points at one valid entry for the duration of the call.
    if unsafe { libc::poll(&mut poll, 1, TICK_MS) } <= 0 {
        return;
    }
    loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                log::warn!("accept failed: {e}");
                return;
            }
        };
        let host = host.clone();
        host.clients.fetch_add(1, Ordering::SeqCst);
        let spawned = std::thread::Builder::new().name("host-conn".into()).spawn({
            let host = host.clone();
            move || {
                serve(&host, stream);
                host.clients.fetch_sub(1, Ordering::SeqCst);
            }
        });
        if let Err(e) = spawned {
            host.clients.fetch_sub(1, Ordering::SeqCst);
            log::warn!("cannot start a connection thread: {e}");
        }
    }
}

fn listener_fd(listener: &Listener) -> libc::c_int {
    match listener {
        Listener::UdSocket(socket) => socket.as_fd().as_raw_fd(),
    }
}

/// Terminate every running session and wait for the children to be reaped.
/// Synchronous on purpose: the process exits right after, and a SIGKILL left
/// to a timer thread would never be sent.
fn end_sessions(host: &Host) {
    let running = host.running();
    if running.is_empty() {
        return;
    }
    log::info!("terminating {} running session(s)", running.len());
    for session in &running {
        session.terminate();
    }
    let grace = Instant::now() + KILL_GRACE;
    while Instant::now() < grace && running.iter().any(|session| session.group_alive()) {
        std::thread::sleep(Duration::from_millis(10));
    }
    for session in &running {
        session.kill_now();
    }
    let deadline = Instant::now() + REAP_DEADLINE;
    while Instant::now() < deadline
        && running
            .iter()
            .any(|session| session.exit_status().is_none())
    {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn prepare_socket_dir(path: &Path, owned: bool) -> Result<(), HostError> {
    let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return Err(HostError::Config(format!(
            "socket path {} has no directory",
            path.display()
        )));
    };
    if owned {
        std::fs::create_dir_all(dir)?;
        // The directory may sit under a shared temporary directory. Mode
        // 0700 keeps other users from reaching the socket at all, before
        // the socket's own mode and the peer check are even asked.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        return Ok(());
    }
    // A directory the caller chose is the caller's: it is not created and
    // its mode is not changed. It only has to be one where another user
    // cannot swap the socket for their own.
    let mode = std::fs::metadata(dir)
        .ok()
        .filter(|meta| meta.is_dir())
        .map(|meta| meta.permissions().mode());
    match mode {
        Some(mode) if mode & 0o022 == 0 || mode & 0o1000 != 0 => Ok(()),
        _ => Err(HostError::Config(format!(
            "socket directory {} is missing, or writable by others without the sticky bit",
            dir.display()
        ))),
    }
}

fn take_lock(socket_path: &Path) -> Result<File, HostError> {
    let mut lock_path = socket_path.as_os_str().to_owned();
    lock_path.push(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(PathBuf::from(lock_path))?;
    // `flock` and not `fcntl`: it belongs to the open file, so two hosts in
    // one process exclude each other too, and the kernel releases it when
    // the host dies however it dies.
    // SAFETY: `file` is an open descriptor for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(if error.kind() == io::ErrorKind::WouldBlock {
            HostError::AlreadyRunning(socket_path.to_path_buf())
        } else {
            HostError::Io(error)
        });
    }
    Ok(file)
}

fn socket_inode(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path).ok().map(|meta| meta.ino())
}

/// Bind the socket, replacing a socket file left by a host that is gone.
/// Returns the listener and the inode of its file.
fn bind(path: &Path) -> Result<(Listener, u64), HostError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)?,
        // Whatever it is, it is not ours to delete.
        Ok(_) => {
            return Err(HostError::Config(format!(
                "{} exists and is not a socket",
                path.display()
            )))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let listener = ListenerOptions::new()
        .name(path.to_fs_name::<GenericFilePath>()?)
        // The file is removed by hand at exit, and only if it is still the
        // one bound here; the default would unlink the path on drop even
        // after something else was put there.
        .reclaim_name(false)
        .nonblocking(ListenerNonblockingMode::Accept)
        .create_sync()?;
    // The socket's mode is the trust boundary. If it cannot be set, the
    // file keeps whatever the umask gave it, so refuse to serve.
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(path);
        return Err(e.into());
    }
    let inode = socket_inode(path).ok_or_else(|| {
        HostError::Io(io::Error::other(
            "the socket file vanished right after bind",
        ))
    })?;
    Ok((listener, inode))
}

/// True when the peer runs as the same user as the host. A platform that
/// cannot say who the peer is leaves the file modes as the only check, as
/// the app's own socket does.
fn peer_is_us(stream: &Stream) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let ours = unsafe { libc::geteuid() };
    match stream.peer_creds().map(|creds| creds.euid()) {
        Ok(Some(theirs)) => theirs == ours,
        Ok(None) => {
            log::warn!("peer credentials carry no user id here; relying on file modes");
            true
        }
        Err(e) => {
            log::warn!("cannot read peer credentials ({e}); relying on file modes");
            true
        }
    }
}

/// The sending half of a connection, shared by the connection's thread and
/// the threads streaming to it.
struct Wire {
    half: Mutex<SendHalf>,
    broken: AtomicBool,
}

impl Wire {
    /// Write one frame. False once the connection is unusable.
    fn send(&self, write: impl FnOnce(&mut SendHalf) -> io::Result<()>) -> bool {
        if self.broken.load(Ordering::SeqCst) {
            return false;
        }
        let mut half = lock(&self.half);
        if write(&mut half).is_err() {
            self.broken.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }

    fn json<T: Serialize>(&self, kind: u8, value: &T) -> bool {
        self.send(|half| protocol::write_json(half, kind, value))
    }

    fn error(&self, code: &str, message: impl Into<String>) {
        self.json(protocol::KIND_ERROR, &WireError::new(code, message));
    }
}

/// One attached stream of a connection.
struct Follower {
    session: Arc<Session>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Follower {
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Taken and released so the flag cannot slip in between the
        // follower's check of it and its going to sleep.
        drop(lock(&self.session.shared));
        self.session.changed.notify_all();
    }
}

fn serve(host: &Arc<Host>, stream: Stream) {
    let allowed = peer_is_us(&stream);
    // On macOS and the BSDs an accepted socket inherits the listener's
    // non-blocking flag. Left set, every read would return at once and this
    // thread would spin, and a write to a slow client would fail instead of
    // waiting.
    if stream.set_nonblocking(false).is_err() || stream.set_recv_timeout(Some(READ_TICK)).is_err() {
        return;
    }
    let (mut recv, send) = stream.split();
    let wire = Arc::new(Wire {
        half: Mutex::new(send),
        broken: AtomicBool::new(false),
    });
    if !allowed {
        log::warn!("refused a connection from another user");
        wire.error(
            protocol::ERR_FORBIDDEN,
            "this session host belongs to another user",
        );
        return;
    }
    if host.clients.load(Ordering::SeqCst) > host.max_connections {
        log::warn!("refused a connection: {} are open", host.max_connections);
        wire.error(
            protocol::ERR_BUSY,
            "the session host has too many connections",
        );
        return;
    }

    let deadline = Instant::now() + HELLO_DEADLINE;
    let hello = protocol::read_frame_patiently(&mut recv, || {
        Instant::now() < deadline && !host.stopping.load(Ordering::SeqCst)
    });
    let peer: Hello = match hello {
        Ok(Frame {
            kind: protocol::KIND_HELLO,
            body,
        }) => match protocol::decode_json(&body) {
            Ok(peer) => peer,
            Err(e) => {
                wire.error(protocol::ERR_PROTOCOL, e.to_string());
                return;
            }
        },
        Ok(frame) => {
            wire.error(
                protocol::ERR_PROTOCOL,
                format!("expected a hello, got frame kind {}", frame.kind),
            );
            return;
        }
        Err(e) => {
            refuse_frame(&wire, &e);
            return;
        }
    };
    match protocol::negotiate(&host.hello, &peer) {
        Ok(_) => {}
        Err(side) => {
            let message = match side {
                Incompatible::PeerTooOld => format!(
                    "this client is too old: it speaks contract {}, the session host needs at least {}",
                    peer.contract, host.hello.min_contract
                ),
                Incompatible::LocalTooOld => format!(
                    "the session host is too old: it speaks contract {}, this client needs at least {}",
                    host.hello.contract, peer.min_contract
                ),
            };
            log::warn!("refused a client of version {:?}: {message}", peer.version);
            wire.error(protocol::ERR_INCOMPATIBLE, message);
            return;
        }
    }
    if !wire.json(protocol::KIND_HELLO, &host.hello) {
        return;
    }

    let mut connection = Connection {
        host: host.clone(),
        wire: wire.clone(),
        followers: HashMap::new(),
    };
    loop {
        let frame =
            protocol::read_frame_patiently(&mut recv, || !host.stopping.load(Ordering::SeqCst));
        match frame {
            Ok(frame) => {
                if !connection.handle(frame) {
                    break;
                }
            }
            Err(e) => {
                refuse_frame(&wire, &e);
                break;
            }
        }
    }
    // Not joined: a follower may be blocked writing to a client that
    // stopped reading, and it ends by itself when the socket closes.
    for follower in connection.followers.values() {
        follower.stop();
    }
    if !connection.followers.is_empty() {
        log::info!(
            "a client disconnected; {} stream(s) detached",
            connection.followers.len()
        );
    }
}

/// Tell the peer why its frame ends the connection, when there is something
/// to tell.
fn refuse_frame(wire: &Wire, error: &FrameError) {
    match error {
        FrameError::Closed | FrameError::Stopped | FrameError::Io(_) => {}
        other => {
            log::warn!("dropping a connection: {other}");
            wire.error(protocol::ERR_PROTOCOL, other.to_string());
        }
    }
}

struct Connection {
    host: Arc<Host>,
    wire: Arc<Wire>,
    followers: HashMap<SessionId, Follower>,
}

type Reply = Result<Value, WireError>;

fn args<T: DeserializeOwned>(value: Value) -> Result<T, WireError> {
    // Absent arguments are an empty object, so a request whose arguments
    // are all optional may leave them out.
    let value = match value {
        Value::Null => Value::Object(Default::default()),
        other => other,
    };
    serde_json::from_value(value)
        .map_err(|e| WireError::new(protocol::ERR_BAD_REQUEST, e.to_string()))
}

fn result<T: Serialize>(value: &T) -> Reply {
    serde_json::to_value(value).map_err(|e| WireError::new(protocol::ERR_PROTOCOL, e.to_string()))
}

impl Connection {
    /// Handle one frame. False ends the connection.
    fn handle(&mut self, frame: Frame) -> bool {
        match frame.kind {
            protocol::KIND_REQUEST => {
                let request: RequestEnvelope = match protocol::decode_json(&frame.body) {
                    Ok(request) => request,
                    Err(e) => {
                        // Without an id there is no reply to address.
                        refuse_frame(&self.wire, &e);
                        return false;
                    }
                };
                let id = request.id;
                // `attach` writes its own reply, because the stream must
                // not start before it.
                if request.op == protocol::OP_ATTACH {
                    return self.attach(id, request.args);
                }
                let reply = self.dispatch(&request.op, request.args);
                self.reply(id, reply)
            }
            protocol::KIND_INPUT => match protocol::decode_input(&frame.body) {
                Ok((id, bytes)) => {
                    if let Some(session) = self.host.find(id) {
                        session.input(bytes);
                    }
                    true
                }
                Err(e) => {
                    refuse_frame(&self.wire, &e);
                    false
                }
            },
            // A frame kind this host does not know: by the contract, one it
            // may skip.
            _ => true,
        }
    }

    fn reply(&self, id: u64, reply: Reply) -> bool {
        let envelope = match reply {
            Ok(result) => ReplyEnvelope {
                id,
                ok: true,
                error: None,
                result,
            },
            Err(error) => ReplyEnvelope {
                id,
                ok: false,
                error: Some(error),
                result: Value::Null,
            },
        };
        self.wire.json(protocol::KIND_REPLY, &envelope)
    }

    fn session(&self, id: SessionId) -> Result<Arc<Session>, WireError> {
        self.host
            .find(id)
            .ok_or_else(|| WireError::new(protocol::ERR_NO_SESSION, format!("no session {id}")))
    }

    fn dispatch(&mut self, op: &str, raw: Value) -> Reply {
        match op {
            protocol::OP_SPAWN => {
                let SpawnArgs { spec } = args(raw)?;
                if self.host.stopping.load(Ordering::SeqCst)
                    || self.host.shutdown_requested.load(Ordering::SeqCst)
                {
                    return Err(WireError::new(
                        protocol::ERR_BUSY,
                        "the session host is shutting down",
                    ));
                }
                let session = Session::spawn(spec, &self.host.settings)
                    .map_err(|message| WireError::new(protocol::ERR_SPAWN, message))?;
                lock(&self.host.sessions).push(session.clone());
                // A shutdown may have listed the sessions to end while this
                // one was being started; it would then outlive the host.
                if self.host.stopping.load(Ordering::SeqCst) {
                    session.kill_now();
                    return Err(WireError::new(
                        protocol::ERR_BUSY,
                        "the session host is shutting down",
                    ));
                }
                result(&SpawnResult {
                    session: session.id,
                    pid: session.pid,
                })
            }
            protocol::OP_LIST => {
                let sessions = lock(&self.host.sessions)
                    .iter()
                    .map(|session| session.info())
                    .collect();
                result(&ListResult { sessions })
            }
            protocol::OP_DETACH => {
                let SessionArgs { session } = args(raw)?;
                if let Some(mut follower) = self.followers.remove(&session) {
                    follower.stop();
                    // Waited for, so that nothing of this stream can arrive
                    // after the reply and be taken for part of the next
                    // attach to the same session.
                    if let Some(thread) = follower.thread.take() {
                        let _ = thread.join();
                    }
                    log::info!("session {session}: a client detached");
                }
                Ok(Value::Null)
            }
            protocol::OP_RESIZE => {
                let ResizeArgs {
                    session,
                    cols,
                    rows,
                } = args(raw)?;
                self.session(session)?
                    .resize(cols, rows)
                    .map_err(|message| WireError::new(protocol::ERR_SESSION_EXITED, message))?;
                Ok(Value::Null)
            }
            protocol::OP_KILL => {
                let SessionArgs { session } = args(raw)?;
                let session = self.session(session)?;
                log::info!("session {}: kill requested", session.id);
                session.kill();
                Ok(Value::Null)
            }
            protocol::OP_FORGET => {
                let SessionArgs { session } = args(raw)?;
                let found = self.session(session)?;
                if found.exit_status().is_none() {
                    return Err(WireError::new(
                        protocol::ERR_SESSION_LIVE,
                        format!("session {session} is still running; kill it first"),
                    ));
                }
                lock(&self.host.sessions).retain(|other| other.id != session);
                found.release_log();
                log::info!("session {session}: forgotten");
                Ok(Value::Null)
            }
            protocol::OP_SHUTDOWN => {
                let ShutdownArgs { force } = args(raw)?;
                let running = self.host.running().len();
                if running > 0 && !force {
                    return Err(WireError::new(
                        protocol::ERR_LIVE_SESSIONS,
                        format!("{running} session(s) still running"),
                    ));
                }
                log::info!("shutdown requested (force: {force}, running: {running})");
                self.host.shutdown_requested.store(true, Ordering::SeqCst);
                Ok(Value::Null)
            }
            other => Err(WireError::new(
                protocol::ERR_UNKNOWN_OP,
                format!("this session host does not know the request `{other}`"),
            )),
        }
    }

    fn attach(&mut self, id: u64, raw: Value) -> bool {
        let prepared = (|| -> Result<(Arc<Session>, AttachResult), WireError> {
            let AttachArgs { session, from_seq } = args(raw)?;
            let found = self.session(session)?;
            if let Some(previous) = self.followers.get(&session) {
                if !previous.done.load(Ordering::SeqCst) {
                    return Err(WireError::new(
                        protocol::ERR_ALREADY_ATTACHED,
                        format!("this connection already follows session {session}"),
                    ));
                }
                self.followers.remove(&session);
            }
            let mut shared = lock(&found.shared);
            // "From the start" is sequence 0: if the log no longer reaches
            // back that far, the client is told its replay is incomplete.
            let (start_seq, gap) = shared.log.resolve(from_seq.unwrap_or(0));
            let (cols, rows) = shared.log.size_at(start_seq);
            shared.attached += 1;
            drop(shared);
            Ok((
                found,
                AttachResult {
                    session,
                    start_seq,
                    gap,
                    cols,
                    rows,
                },
            ))
        })();
        let (session, attached) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.reply(id, Err(error)),
        };
        log::info!(
            "session {}: a client attached at {}{}",
            session.id,
            attached.start_seq,
            if attached.gap {
                " (the requested position was already trimmed)"
            } else {
                ""
            }
        );
        let start_seq = attached.start_seq;
        if !self.reply(id, result(&attached)) {
            lock(&session.shared).attached -= 1;
            return false;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("host-follow".into())
            .spawn({
                let (session, wire) = (session.clone(), self.wire.clone());
                let (stop, done) = (stop.clone(), done.clone());
                move || {
                    follow(&session, &wire, &stop, start_seq);
                    lock(&session.shared).attached -= 1;
                    done.store(true, Ordering::SeqCst);
                }
            });
        match thread {
            Ok(thread) => {
                self.followers.insert(
                    session.id,
                    Follower {
                        session,
                        stop,
                        done,
                        thread: Some(thread),
                    },
                );
                true
            }
            Err(e) => {
                // The client was already told the stream starts; the only
                // honest way out is to end the connection.
                lock(&session.shared).attached -= 1;
                self.wire.error(
                    protocol::ERR_PROTOCOL,
                    format!("cannot start the stream thread: {e}"),
                );
                false
            }
        }
    }
}

enum Step {
    Batch(crate::log::Batch),
    Exited(protocol::ExitStatus),
    Stop,
}

/// Body of a stream thread: send the log from `cursor` on, then whatever is
/// appended, until the session ends or the stream is stopped.
///
/// All the thread holds is `cursor`. It never queues anything: when the
/// client is slow it is the thread that waits in the socket write, the
/// session's reader goes on filling the log, and if the log wraps past
/// `cursor` meanwhile the client is told what it lost and the stream goes
/// on from the head.
fn follow(session: &Arc<Session>, wire: &Wire, stop: &AtomicBool, mut cursor: u64) {
    let id = session.id;
    // A client slower than the session can fall behind again on every
    // batch, so only the first gap of a stream gets a line of its own.
    let (mut gaps, mut lost) = (0u64, 0u64);
    let summarise = |gaps: u64, lost: u64| {
        if gaps > 1 {
            log::info!("session {id}: a client fell behind {gaps} times, {lost} positions lost");
        }
    };
    loop {
        let step = {
            let mut shared = lock(&session.shared);
            loop {
                if stop.load(Ordering::SeqCst) || wire.broken.load(Ordering::SeqCst) {
                    break Step::Stop;
                }
                let batch = shared.log.read_from(cursor, protocol::MAX_DATA_CHUNK);
                if batch.gap || !batch.items.is_empty() {
                    break Step::Batch(batch);
                }
                if let Some(status) = shared.exit {
                    break Step::Exited(status);
                }
                // The timeout is a second chance at noticing a broken
                // connection; every real change is announced.
                shared = session
                    .changed
                    .wait_timeout(shared, Duration::from_secs(1))
                    .map(|(guard, _)| guard)
                    .unwrap_or_else(|poisoned| poisoned.into_inner().0);
            }
        };
        let batch = match step {
            Step::Stop => break,
            Step::Exited(status) => {
                wire.json(protocol::KIND_EVENT, &EventBody::exited(id, status));
                break;
            }
            Step::Batch(batch) => batch,
        };
        if batch.gap {
            gaps += 1;
            lost += batch.start_seq - cursor;
            if gaps == 1 {
                log::info!(
                    "session {id}: a client fell behind the log, {} positions lost",
                    batch.start_seq - cursor
                );
            }
            if !wire.json(
                protocol::KIND_EVENT,
                &EventBody::gap(id, cursor, batch.start_seq),
            ) {
                break;
            }
        }
        if !send_items(wire, id, &batch.items) {
            break;
        }
        cursor = batch.next_seq;
    }
    summarise(gaps, lost);
}

/// Write a batch of log items as stream frames. False once the connection
/// is unusable.
fn send_items(wire: &Wire, id: SessionId, items: &[Item]) -> bool {
    items.iter().all(|item| match item {
        Item::Output { seq, bytes } => {
            wire.send(|half| protocol::write_output(half, id, *seq, bytes))
        }
        Item::Resize { seq, cols, rows } => wire.json(
            protocol::KIND_EVENT,
            &EventBody::resized(id, *seq, *cols, *rows),
        ),
    })
}
