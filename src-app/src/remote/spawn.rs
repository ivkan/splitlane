//! The bridge the window starts, and how the window knows a request is its.
//!
//! A phone may write into agent sessions because it was paired: pairing,
//! with its code typed on this computer, is the person's consent. The
//! window then has to tell the bridge's requests from any other process's, and a claim it could check by reading a file
//! (the paired phones, or a token on disk) would be one any process of
//! the user's could make, and the journal would credit its writes to a
//! phone. So the window starts the bridge itself and hands it a random
//! token over the bridge's standard input, kept in memory on both sides
//! and nowhere else. A request that carries it is the bridge's.
//!
//! The same pipe is the bridge's lifeline: it reads until the window's end
//! closes, and leaves then. A bridge started by hand with `splitlane remote
//! serve` has no token, and reads but does not write.
//!
//! This does not make the window proof against a program of the user's
//! that reads another process's memory. Nothing short of the system can.

use std::io::{BufRead, Write};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use serde_json::Value;

/// argv[1] of the bridge the window starts.
pub(crate) const BRIDGE_SUBCOMMAND: &str = "__splitlane-bridge";

/// What a request presents to be the window's bridge.
pub(crate) const TOKEN_PARAM: &str = "bridge_token";
/// The paired phone a write comes from: its name, for the window's notice.
pub(crate) const DEVICE_PARAM: &str = "paired_device";
/// Set by the IPC thread, and only there, when the token matched. A request
/// that arrives carrying it has it taken out first.
pub(crate) const VERIFIED_PARAM: &str = "paired_device_verified";

static TOKEN: OnceLock<String> = OnceLock::new();

/// In the IPC thread, before a request reaches the window: take out any
/// verified mark the caller wrote itself, and put one in when the request
/// carries the bridge's token.
pub(crate) fn mark_bridge_request(params: &mut Value) {
    let Some(fields) = params.as_object_mut() else {
        return;
    };
    fields.remove(VERIFIED_PARAM);
    let presented = fields
        .remove(TOKEN_PARAM)
        .and_then(|token| token.as_str().map(str::to_string));
    let (Some(presented), Some(held)) = (presented, TOKEN.get()) else {
        return;
    };
    if !splitlane_remote::code_matches_bytes(presented.as_bytes(), held.as_bytes()) {
        return;
    }
    let device = fields
        .get(DEVICE_PARAM)
        .and_then(Value::as_str)
        .unwrap_or("a phone")
        .to_string();
    fields.insert(VERIFIED_PARAM.into(), Value::String(device));
}

/// The phone a request was verified to come from, in the window.
pub(crate) fn verified_device(params: &Value) -> Option<&str> {
    params.get(VERIFIED_PARAM).and_then(Value::as_str)
}

/// Start the bridge when a phone is paired, with the window's token. The
/// child is kept by the caller: dropping its standard input ends it.
pub(crate) fn start_bridge_if_paired() -> Option<Child> {
    if !phone_paired() {
        return None;
    }
    let token = TOKEN.get_or_init(new_token);
    spawn_bridge(token, false)
}

/// Whether a window of this build answers on its IPC socket.
fn window_answers() -> bool {
    use splitlane_ipc_client::IpcTransport as _;
    splitlane_ipc_client::resolve_socket_path().is_some_and(|socket| {
        splitlane_ipc_client::IpcClient::new(socket)
            .call("system.ping", serde_json::json!({}))
            .is_ok()
    })
}

fn phone_paired() -> bool {
    super::store::dir()
        .and_then(|dir| super::store::devices(&dir).ok())
        .is_some_and(|devices| !devices.is_empty())
}

/// A random token, as hex.
pub(crate) fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `for_host` marks a bridge the session host started: it leaves as soon as
/// a window answers, because that window starts a bridge of its own.
fn spawn_bridge(token: &str, for_host: bool) -> Option<Child> {
    let exe = std::env::current_exe().ok()?;
    let mut child = Command::new(exe)
        .arg(BRIDGE_SUBCOMMAND)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| log::warn!("cannot start the phone bridge: {e}"))
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let owner = if for_host { "host" } else { "window" };
    if writeln!(stdin, "{token}\n{owner}")
        .and_then(|()| stdin.flush())
        .is_err()
    {
        let _ = child.kill();
        return None;
    }
    child.stdin = Some(stdin);
    Some(child)
}

/// In the session host: keep the phone bridge running while no window is,
/// so a paired phone can still reply. A window starts a bridge of its own,
/// and two in one relay room keep starting each other's channel over, so
/// this one is stopped as soon as a window is attached to a session again.
/// `token` is the one the host takes writes with.
pub(crate) fn keep_host_bridge(socket: std::path::PathBuf, token: String) {
    let mut bridge: Option<Child> = None;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        // A window that answers counts even when it shows no session of
        // this host: it has started a bridge of its own.
        let window_attached = window_answers()
            || match splitlane_host::Client::connect(&socket).and_then(|client| client.list()) {
                Ok(sessions) => sessions.iter().any(|session| session.attached > 0),
                Err(e) => {
                    log::debug!("phone bridge keeper: cannot list sessions ({e})");
                    continue;
                }
            };
        if let Some(child) = bridge.as_mut() {
            let gone = matches!(child.try_wait(), Ok(Some(_)) | Err(_));
            if gone || window_attached {
                // Closing its input is what ends it; see the module docs.
                drop(child.stdin.take());
                let _ = child.wait();
                log::info!("phone bridge of the session host stopped");
                bridge = None;
            }
        } else if !window_attached && phone_paired() {
            bridge = spawn_bridge(&token, true);
            if bridge.is_some() {
                log::info!("phone bridge of the session host started: no window is attached");
            }
        }
    }
}

fn rand_bytes(out: &mut [u8]) {
    // The same source the pairing keys come from.
    splitlane_remote::fill_random(out);
}

/// Body of the bridge the window started: read the token, follow the pipe
/// for the window's end, and serve.
pub(crate) fn run_spawned_bridge() -> i32 {
    let stdin = std::io::stdin();
    let mut token = String::new();
    if stdin.lock().read_line(&mut token).unwrap_or(0) == 0 {
        return 2;
    }
    let token = token.trim().to_string();
    let mut owner = String::new();
    let _ = stdin.lock().read_line(&mut owner);
    // A host's bridge gives way to a window's, which the window starts.
    if owner.trim() == "host" {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3));
                if window_answers() {
                    std::process::exit(0);
                }
            }
        });
    }
    // The window holds the other end. When it closes - the window quit or
    // died - nobody can write through this bridge any more, so it goes.
    std::thread::spawn(move || {
        let mut rest = String::new();
        while stdin.lock().read_line(&mut rest).unwrap_or(0) > 0 {
            rest.clear();
        }
        std::process::exit(0);
    });
    match super::bridge::serve_with(Some(token)) {
        Ok(()) => 0,
        Err(message) => {
            log::warn!("phone bridge: {message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_verified_mark_from_outside_is_taken_out() {
        let mut params = json!({ VERIFIED_PARAM: "Pixel", "text": "rm -rf ~" });
        mark_bridge_request(&mut params);
        assert_eq!(verified_device(&params), None);
    }

    #[test]
    fn only_the_held_token_marks_a_request() {
        let held = TOKEN.get_or_init(|| "a".repeat(64)).clone();
        let mut wrong = json!({ TOKEN_PARAM: "b".repeat(64), DEVICE_PARAM: "Pixel" });
        mark_bridge_request(&mut wrong);
        assert_eq!(verified_device(&wrong), None);
        let mut right = json!({ TOKEN_PARAM: held, DEVICE_PARAM: "Pixel" });
        mark_bridge_request(&mut right);
        assert_eq!(verified_device(&right), Some("Pixel"));
        // The token never travels further than the IPC thread.
        assert!(right.get(TOKEN_PARAM).is_none());
    }
}
