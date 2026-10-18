//! The session host: a long-lived process without a window that owns the
//! PTYs of terminal sessions, so that closing, updating or crashing the
//! window does not end what runs in them.
//!
//! A client connects over a local socket, asks for a session's output log
//! from some position, replays it through its own terminal emulator and then
//! follows the live bytes. The host keeps raw output, not a screen, so it
//! needs no agreement with the client about how a screen is represented.
//!
//! It does run an emulator of its own for each session, for one purpose: a
//! program asks its terminal questions and waits for the answers, and it
//! must get them when no window is attached. The host answers; a client's
//! emulator only draws.
//!
//! - [`protocol`]: the wire contract, versioned on its own because a host
//!   and its clients are routinely different releases.
//! - [`log`]: the bounded per-session output log.
//! - [`scan`]: where that log may be cut, and which terminal modes a client
//!   that starts reading there has to be put into.
//! - [`engine`]: the emulator that answers the program.
//! - [`program_status`]: what a program reports about itself, and the answer
//!   to its question whether anybody reads that.
//! - [`client`]: a blocking client, and [`client::ensure_running`] to start a
//!   host when none answers.
//! - [`run`]: the host itself.
//!
//! The host runs on Unix. On other platforms the crate compiles and its
//! entry points return [`HostError::Unsupported`].

use std::path::PathBuf;
use std::time::Duration;

pub mod client;
pub mod engine;
pub mod log;
pub mod program_status;
pub mod protocol;
pub mod scan;
pub mod transcript;

#[cfg(unix)]
mod descriptors;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod session;

pub use client::{ensure_running, AttachFrom, Attachment, Client, StreamEvent};
#[cfg(unix)]
pub use descriptors::{close_inherited_descriptors, raise_descriptor_limit};
pub use protocol::{ExitStatus, Facts, Rgb, SessionId, SessionInfo, SpawnSpec};

/// Lock a mutex, carrying on after a panic in another holder. Everything
/// guarded in this crate stays consistent between statements, and a host
/// that stops answering takes every session with it.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Argv[1] that makes the app binary run as a host instead of opening a
/// window.
pub const HOST_SUBCOMMAND: &str = "__splitlane-host";

/// Largest socket path the host accepts, in bytes. `sockaddr_un.sun_path`
/// holds 104 bytes on macOS and 108 on Linux, including the terminating
/// NUL; the smaller one is used everywhere so a path that works on one
/// works on the other.
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("the session host is not supported on this platform yet")]
    Unsupported,
    /// Nothing is listening on the socket.
    #[error("no session host is running")]
    NotRunning,
    #[error("a session host is already running on {0}")]
    AlreadyRunning(PathBuf),
    /// The two sides have no contract in common. The message says which
    /// side is too old.
    #[error("{0}")]
    Incompatible(String),
    /// The host answered a request with an error.
    #[error("{message}")]
    Remote { code: String, message: String },
    #[error("{0}")]
    Protocol(String),
    #[error("timed out {0}")]
    Timeout(String),
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl HostError {
    /// The error code of a refused request, if that is what this is.
    pub fn remote_code(&self) -> Option<&str> {
        match self {
            HostError::Remote { code, .. } => Some(code),
            _ => None,
        }
    }
}

/// Nothing is inherited that a host could hold on to on this platform yet.
#[cfg(not(unix))]
pub fn close_inherited_descriptors() {}

/// No host runs on this platform yet, so there is no limit of its to raise.
#[cfg(not(unix))]
pub fn raise_descriptor_limit() {}

/// How a host is run. Built with [`HostConfig::new`] and adjusted field by
/// field, so a field added later does not break a caller.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HostConfig {
    pub socket_path: PathBuf,
    /// Whether the socket's directory is the host's to create and to
    /// restrict to the user. False for a path the caller chose: then the
    /// directory must already exist and must not be writable by others.
    pub owns_socket_dir: bool,
    /// Capacity of one session's output log.
    pub log_capacity_bytes: usize,
    /// Bound on all logs together. When their sum goes over it, the largest
    /// log is cut first, by the excess but by no more than half of itself,
    /// and then the next largest.
    pub total_log_capacity_bytes: usize,
    /// How long the host stays with no running session and no connected
    /// client before it exits.
    pub idle_grace: Duration,
    /// How long a session's process group gets between being hung up on
    /// and being killed.
    pub kill_grace: Duration,
    /// The binary that implements the kill-on-host-death watcher. `None`
    /// starts no watcher, which is what tests want.
    pub guard_exe: Option<PathBuf>,
    /// Connections served at once; one more is told the host is busy. A
    /// window holds one for each pane and one for itself, so this bounds
    /// the panes a host serves, and it is set where the descriptors would
    /// run out anyway: see [`raise_descriptor_limit`].
    pub max_connections: usize,
    /// Treat SIGTERM and SIGINT as a forced shutdown. Process-wide, so it
    /// is off unless the host is the whole process.
    pub handle_signals: bool,
    /// Release version reported in the handshake. For display only.
    pub version: String,
    /// Whether `send_text` may write into a session: the setting the window
    /// gates its own writes with, `SPLITLANE_IPC_SCRIPTING`, as it stood in
    /// the Splitlane that started this host. Kept for the host's life.
    pub writes_allowed: bool,
    /// The token of the phone bridge this host started, if it started one.
    /// A write that presents it is let through without `writes_allowed`,
    /// into an agent's session only: a paired phone writes by pairing.
    pub bridge_token: Option<String>,
}

/// How long a process group gets between the hangup and the kill unless a
/// host is configured otherwise. The same as a pane that runs in the
/// window's own process gives.
const KILL_GRACE: Duration = Duration::from_millis(100);

impl HostConfig {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            owns_socket_dir: true,
            log_capacity_bytes: log::DEFAULT_CAPACITY,
            total_log_capacity_bytes: 128 * 1024 * 1024,
            idle_grace: Duration::from_secs(10),
            kill_grace: KILL_GRACE,
            guard_exe: None,
            max_connections: 1024,
            handle_signals: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
            writes_allowed: false,
            bridge_token: None,
        }
    }
}

/// Run a host on `config.socket_path` until it is told to stop or has been
/// idle for the grace period. Returns after the socket is removed.
pub fn run(config: HostConfig) -> Result<(), HostError> {
    #[cfg(unix)]
    {
        server::run(config)
    }
    #[cfg(not(unix))]
    {
        let _ = config;
        Err(HostError::Unsupported)
    }
}
