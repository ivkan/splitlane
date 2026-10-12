//! `splitlane host status` and `splitlane host stop`.
//!
//! The session host keeps terminal sessions running without a window. It
//! has a socket of its own, so these two verbs work whether or not a
//! Splitlane window is open, and they never start a host: asking about one
//! must not bring it into being.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use splitlane_host::{Client, HostError, SessionInfo};

use super::{CliError, EXIT_OK, EXIT_REFUSED};

/// How long `stop` waits for the host to be gone after it agreed to go.
const STOP_DEADLINE: Duration = Duration::from_secs(5);

fn runtime(error: HostError) -> CliError {
    CliError::runtime(format!("session host: {error}"))
}

/// A connection to this build's host, or `None` when none runs. A platform
/// without a host is the same answer: there is nothing to ask.
fn connect(socket: &Path) -> Result<Option<Client>, CliError> {
    match Client::connect(socket) {
        Ok(client) => Ok(Some(client)),
        Err(HostError::NotRunning | HostError::Unsupported) => Ok(None),
        Err(e) => Err(runtime(e)),
    }
}

fn socket() -> Result<std::path::PathBuf, CliError> {
    crate::runtime_paths::host_socket_path_spec()
        .map(|spec| spec.path().to_path_buf())
        .ok_or_else(|| CliError::runtime("cannot work out where the session host socket belongs"))
}

fn state(session: &SessionInfo) -> String {
    match session.exited {
        None => "running".into(),
        Some(status) => match (status.code, status.signal) {
            (_, Some(signal)) => format!("exited (signal {signal})"),
            (Some(code), None) => format!("exited (code {code})"),
            (None, None) => "exited".into(),
        },
    }
}

/// `splitlane host status [--json]`.
pub fn status(as_json: bool) -> Result<i32, CliError> {
    let socket = socket()?;
    let Some(client) = connect(&socket)? else {
        if as_json {
            super::print_json(&json!({ "running": false, "socket": socket }))?;
        } else {
            println!("session host: not running");
        }
        return Ok(EXIT_OK);
    };
    let sessions = client.list().map_err(runtime)?;
    let host = client.host();
    if as_json {
        super::print_json(&json!({
            "running": true,
            "socket": socket,
            "pid": host.pid,
            "version": host.version,
            "contract": host.contract,
            "sessions": sessions,
        }))?;
        return Ok(EXIT_OK);
    }
    println!(
        "session host: running (pid {}, version {})",
        host.pid
            .map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
        host.version
    );
    for session in &sessions {
        println!(
            "{}  pid {}  {}x{}  {}  {}  {} bytes",
            session.id,
            session.pid,
            session.cols,
            session.rows,
            state(session),
            session.label.as_deref().unwrap_or("-"),
            session.log_bytes
        );
    }
    Ok(EXIT_OK)
}

/// `splitlane host stop [--force]`.
pub fn stop(force: bool) -> Result<i32, CliError> {
    let socket = socket()?;
    let Some(client) = connect(&socket)? else {
        println!("session host: not running");
        return Ok(EXIT_OK);
    };
    let running = client
        .list()
        .map_err(runtime)?
        .iter()
        .filter(|session| session.exited.is_none())
        .count();
    if running > 0 && !force && !confirm(running)? {
        return Err(CliError {
            code: EXIT_REFUSED,
            message: format!(
                "session host: {running} session(s) still running; nothing was stopped \
                 (pass --force to terminate them)"
            ),
        });
    }
    // Forced only when there was something to force, so a session started
    // since the question was asked is refused by the host and not killed
    // unasked.
    match client.shutdown(running > 0) {
        Ok(()) => {}
        Err(e) if e.remote_code() == Some(splitlane_host::protocol::ERR_LIVE_SESSIONS) => {
            return Err(CliError {
                code: EXIT_REFUSED,
                message: format!("session host: {e}; nothing was stopped"),
            });
        }
        Err(e) => return Err(runtime(e)),
    }
    drop(client);
    let deadline = Instant::now() + STOP_DEADLINE;
    while !matches!(connect(&socket), Ok(None)) {
        if Instant::now() >= deadline {
            return Err(CliError::runtime(
                "session host: agreed to stop but is still answering",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("session host: stopped");
    Ok(EXIT_OK)
}

/// Ask the person at the terminal. Without a terminal there is nobody to
/// ask, and the answer is no.
fn confirm(running: usize) -> Result<bool, CliError> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    eprint!("{running} session(s) are still running and will be terminated. Stop the host? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| CliError::runtime(format!("cannot read the answer: {e}")))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitlane_host::{ExitStatus, SessionId};

    fn session(exited: Option<ExitStatus>) -> SessionInfo {
        serde_json::from_value(json!({ "id": SessionId([1; 16]), "exited": exited }))
            .expect("a session with only an id is valid")
    }

    #[test]
    fn a_session_state_reads_as_running_or_as_how_it_ended() {
        assert_eq!(state(&session(None)), "running");
        assert_eq!(
            state(&session(Some(ExitStatus {
                code: Some(3),
                signal: None
            }))),
            "exited (code 3)"
        );
        assert_eq!(
            state(&session(Some(ExitStatus {
                code: None,
                signal: Some(9)
            }))),
            "exited (signal 9)"
        );
    }
}
