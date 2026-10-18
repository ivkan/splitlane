//! What the bridge answers a phone with: the CLI's own surface.

use serde_json::{Value, json};
use splitlane_ipc_client::{IpcClient, IpcTransport};

use crate::cli::closed_window::ClosedWindow;

/// Longest answer text sent to a phone.
const ANSWER_MAX_BYTES: usize = 32 * 1024;
/// Lines of screen a phone gets when it does not say, and at most.
const READ_LINES: u64 = 200;
const READ_MAX_LINES: u64 = 400;

/// The window when it answers, the session host when it does not; asked
/// again for every request, so a window that opens or closes is followed.
pub(crate) enum Surface {
    Window(IpcClient),
    Closed(Box<ClosedWindow>),
}

impl Surface {
    pub(crate) fn current() -> Result<Self, String> {
        if let Some(socket) = splitlane_ipc_client::resolve_socket_path() {
            let window = IpcClient::new(socket);
            if window.call("system.ping", json!({})).is_ok() {
                return Ok(Surface::Window(window));
            }
        }
        ClosedWindow::read()
            .map(|closed| Surface::Closed(Box::new(closed)))
            .ok_or_else(|| "Splitlane is not running and no session host is".to_string())
    }
}

impl IpcTransport for Surface {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        match self {
            Surface::Window(window) => window.call(method, params),
            Surface::Closed(closed) => closed.call(method, params),
        }
    }
}

/// Answer one request from a phone. Only reading, so far: what the phone
/// may write, and how the window tells that write from any other, comes
/// with the write itself.
pub(crate) fn answer(
    surface: &impl IpcTransport,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    let surface_id = || {
        params
            .get("surface_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{method} needs a surface_id"))
    };
    match method {
        "list" => surface.call("fleet.list", json!({})),
        "status" => surface.call("surface.status", json!({ "surface_id": surface_id()? })),
        "read" => {
            let lines = params
                .get("lines")
                .and_then(Value::as_u64)
                .unwrap_or(READ_LINES)
                .clamp(1, READ_MAX_LINES);
            // Not fenced: the page shows the text as text and runs nothing
            // in it; the fence is for an agent reading another's output.
            surface.call(
                "surface.read",
                json!({ "surface_id": surface_id()?, "lines": lines, "fenced": false }),
            )
        }
        "answer" => crate::cli::answer_cmd::answer_value(surface, surface_id()?, ANSWER_MAX_BYTES),
        other => Err(format!("unknown request `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records what it was asked and answers with the method's name.
    #[derive(Default)]
    struct Recording(RefCell<Vec<(String, Value)>>);

    impl IpcTransport for Recording {
        fn call(&self, method: &str, params: Value) -> Result<Value, String> {
            self.0.borrow_mut().push((method.to_string(), params));
            Ok(json!({ "method": method }))
        }
    }

    #[test]
    fn requests_become_the_clis_calls() {
        let surface = Recording::default();
        answer(&surface, "list", &Value::Null).unwrap();
        answer(&surface, "status", &json!({ "surface_id": 7 })).unwrap();
        answer(&surface, "read", &json!({ "surface_id": 7, "lines": 9999 })).unwrap();
        let calls = surface.0.into_inner();
        assert_eq!(calls[0].0, "fleet.list");
        assert_eq!(
            calls[1],
            ("surface.status".into(), json!({ "surface_id": 7 }))
        );
        assert_eq!(calls[2].0, "surface.read");
        assert_eq!(calls[2].1["lines"], READ_MAX_LINES);
        assert_eq!(calls[2].1["fenced"], false);
    }

    #[test]
    fn a_request_it_does_not_know_or_without_its_pane_is_refused() {
        let surface = Recording::default();
        assert!(answer(&surface, "send", &json!({ "surface_id": 7 })).is_err());
        assert!(answer(&surface, "status", &Value::Null).is_err());
        assert!(surface.0.into_inner().is_empty());
    }
}
