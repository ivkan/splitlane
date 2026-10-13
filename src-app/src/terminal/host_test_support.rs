//! What the tests of a terminal in the session host share, whichever
//! terminal it is: a real host on a temporary socket, programs to run in
//! it, and waits that poll with a deadline.

use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use splitlane_host::{Client, HostConfig, HostError, SessionInfo};

use super::*;
use crate::terminal::host_session::HostEndpoint;

pub(super) const DEADLINE: Duration = Duration::from_secs(20);

/// Poll `probe` until it yields something, or fail naming `what`.
pub(super) fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(found) = probe() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A host running on a thread of the test, on a socket of its own, and the
/// endpoint a window would use to reach it.
pub(super) struct TestHost {
    // Field order matters: the directory is removed last.
    pub(super) thread: Option<JoinHandle<Result<(), HostError>>>,
    pub(super) endpoint: Arc<HostEndpoint>,
    pub(super) socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// A directory short enough for a socket path: macOS allows 104 bytes and
/// its default temporary directory uses half of them.
pub(super) fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("slp")
        .tempdir_in("/tmp")
        .expect("a temporary directory")
}

impl TestHost {
    pub(super) fn start(configure: impl FnOnce(&mut HostConfig)) -> Self {
        let dir = short_dir();
        let socket = dir.path().join("h").join("host.sock");
        let mut config = HostConfig::new(&socket);
        configure(&mut config);
        let thread = std::thread::spawn(move || splitlane_host::run(config));
        let host = TestHost {
            thread: Some(thread),
            // No launcher: the host is this thread, and a test that loses
            // it must fail, not start another.
            endpoint: HostEndpoint::new(socket.clone(), None),
            socket,
            _dir: dir,
        };
        drop(host.client());
        host
    }

    pub(super) fn client(&self) -> Client {
        wait_for("the host to answer", || {
            assert!(
                !self.thread.as_ref().is_some_and(JoinHandle::is_finished),
                "the host exited before answering"
            );
            Client::connect(&self.socket).ok()
        })
    }

    pub(super) fn sessions(&self) -> Vec<SessionInfo> {
        self.client().list().expect("the host lists its sessions")
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        // Over the endpoint's connection if it has one: a host that was
        // told to take no more would refuse a new one.
        if !thread.is_finished() {
            let asked = self
                .endpoint
                .for_existing_session()
                .is_some_and(|control| control.shutdown(true).is_ok());
            if !asked && let Ok(client) = Client::connect(&self.socket) {
                let _ = client.shutdown(true);
            }
        }
        let _ = thread.join();
    }
}

/// What a pane is started with, for a program of the test's choosing in
/// place of the user's shell. Everything else is what a pane resolves.
pub(super) fn pane_params(program: &str, args: &[&str]) -> SpawnParams {
    let mut params =
        TerminalState::resolve_spawn_params(Some(PathBuf::from("/")), 7, 11, Some((80, 24)), None);
    params.shell = program.to_string();
    params.extra_args = args.iter().map(|arg| arg.to_string()).collect();
    params
}

pub(super) fn sh(script: &str) -> SpawnParams {
    pane_params("/bin/sh", &["-c", script])
}

/// The pane every test's terminal is shown in, and the id its shell is
/// told: the one `pane_params` resolves with.
pub(super) const PANE: u64 = 11;

pub(super) fn group_alive(pid: u32) -> bool {
    // SAFETY: signal 0 to a process group only asks whether it has members.
    unsafe { libc::kill(-(pid as libc::pid_t), 0) == 0 }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A shell command that reads exactly `count` bytes of input and prints
/// them as hexadecimal digits. The terminal must be in raw mode.
pub(super) fn read_hex(count: usize) -> String {
    format!("dd bs=1 count={count} 2>/dev/null | od -An -tx1 | tr -d ' \\n'")
}

/// A shell command that prints whatever input arrives within a third of a
/// second, as hexadecimal digits between brackets: empty brackets when the
/// terminal had nothing more to say.
pub(super) const READ_STRAGGLERS: &str = "stty min 0 time 3; printf ' more[%s]' \
     \"$(dd bs=64 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n')\"; stty min 1 time 0";
