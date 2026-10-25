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

/// Keys a phone may press: answering a question or a permission prompt, and
/// moving through its choices. Nothing that edits a line or signals a
/// process beyond what `interrupt` does.
const PHONE_KEYS: &[&str] = &[
    "enter", "escape", "up", "down", "left", "right", "0", "1", "2", "3", "4", "5", "6", "7", "8",
    "9", "y", "n",
];

/// Answer one request from a phone.
///
/// A write is let through only from a bridge the window started (`token`),
/// for a phone paired with the right to reply, and only while the window
/// runs: the window is what checks that the session is an agent's and
/// tells the person. Each write goes into the journal, without its text.
pub(crate) fn answer(
    surface: &impl IpcTransport,
    method: &str,
    params: &Value,
    device: &super::store::Device,
    token: Option<&str>,
    window_open: bool,
) -> Result<Value, String> {
    if matches!(method, "send" | "key" | "interrupt") {
        return write(surface, method, params, device, token, window_open);
    }
    read(surface, method, params)
}

fn write(
    surface: &impl IpcTransport,
    method: &str,
    params: &Value,
    device: &super::store::Device,
    token: Option<&str>,
    window_open: bool,
) -> Result<Value, String> {
    let Some(token) = token else {
        return Err(
            "this bridge was started by hand and only reads; replies go through \
             the bridge Splitlane starts"
                .into(),
        );
    };
    if !device.reply {
        return Err(format!("{} may read but not reply", device.name));
    }
    // Interrupting needs the agent's own stop key, which only the window
    // knows; text and keys go through the session host while it is closed.
    if !window_open && method == "interrupt" {
        return Err("start Splitlane to interrupt from the phone".into());
    }
    let surface_id = params
        .get("surface_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{method} needs a surface_id"))?;
    let mut call = json!({
        "surface_id": surface_id,
        crate::remote::spawn::TOKEN_PARAM: token,
        crate::remote::spawn::DEVICE_PARAM: device.name,
    });
    let (ipc_method, bytes) = match method {
        "send" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            call["text"] = json!(text);
            call["submit"] = json!(params.get("submit").and_then(Value::as_bool) == Some(true));
            ("surface.send_text", text.len())
        }
        "key" => {
            let key = params
                .get("keystroke")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !PHONE_KEYS.contains(&key) {
                return Err(format!("a phone may not press `{key}`"));
            }
            call["keystroke"] = json!(key);
            ("surface.send_keystroke", key.len())
        }
        _ => ("surface.interrupt", 0),
    };
    // Written ahead: a write that cannot be journalled is not made.
    super::journal::record(device, method, surface_id, bytes, "asked")
        .map_err(|e| format!("the phone journal cannot be written ({e}); nothing was sent"))?;
    let result = surface.call(ipc_method, call);
    let outcome = if result.is_ok() { "done" } else { "refused" };
    let _ = super::journal::record(device, method, surface_id, bytes, outcome);
    result
}

fn read(surface: &impl IpcTransport, method: &str, params: &Value) -> Result<Value, String> {
    let surface_id = || {
        params
            .get("surface_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{method} needs a surface_id"))
    };
    match method {
        "list" => {
            // The rail's own list, so the phone groups, names and counts
            // sessions as the window does. Only the window has it; without
            // it the phone lists the running agents by project, as before.
            // Asked before the agents: an agent that starts between the two
            // calls is then listed under its pane's name until the next
            // list, where asked after it would be drawn as not running.
            let rail = surface
                .call("rail.list", json!({}))
                .ok()
                .filter(|rail| rail.get("projects").is_some_and(Value::is_array));
            let mut fleet = surface.call("fleet.list", json!({}))?;
            // Project names, for the phone to group sessions under. Only
            // the window has them; without, the phone falls back on each
            // session's directory.
            if let Ok(listed) = surface.call("workspace.list", json!({})) {
                let projects: Vec<Value> = listed
                    .get("workspaces")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|w| json!({ "index": w.get("index"), "title": w.get("title") }))
                    .collect();
                if let Some(fleet) = fleet.as_object_mut() {
                    fleet.insert("projects".into(), json!(projects));
                }
            }
            // Which Splitlane answers, for the phone to show beside its own
            // version: whether the computer has been updated is then read
            // off the phone rather than asked.
            if let Some(fleet) = fleet.as_object_mut() {
                fleet.insert("app_version".into(), json!(env!("CARGO_PKG_VERSION")));
            }
            // The plan limits the rail's footer shows, where the window has
            // them; the phone says they are unavailable otherwise.
            if let Ok(limits) = surface.call("limits.list", json!({}))
                && limits.get("vendors").is_some_and(Value::is_array)
                && let Some(fleet) = fleet.as_object_mut()
            {
                fleet.insert("limits".into(), limits);
            }
            if let Some(mut rail) = rail {
                let agents = fleet.get("agents").cloned().unwrap_or(Value::Null);
                add_unlisted_agents(&mut rail, &agents);
                if let Some(fleet) = fleet.as_object_mut() {
                    fleet.insert("rail".into(), rail);
                }
            }
            Ok(fleet)
        }
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

/// Put each running agent the rail has no session row for under its
/// project, after the rail's own: an agent somebody started by hand in a
/// pane the rail does not list as a session. Named by its pane, as the phone
/// named every session before it had the rail's names.
fn add_unlisted_agents(rail: &mut Value, agents: &Value) {
    let Some(projects) = rail.get_mut("projects").and_then(Value::as_array_mut) else {
        return;
    };
    let listed: Vec<Value> = projects
        .iter()
        .filter_map(|project| project.get("sessions").and_then(Value::as_array))
        .flatten()
        .filter_map(|session| {
            session
                .get("surface_id")
                .filter(|id| !id.is_null())
                .cloned()
        })
        .collect();
    for agent in agents.as_array().into_iter().flatten() {
        let Some(surface_id) = agent.get("surface_id").filter(|id| !id.is_null()) else {
            continue;
        };
        if listed.contains(surface_id) {
            continue;
        }
        let Some(sessions) = projects
            .iter_mut()
            .find(|project| {
                project.get("index").is_some() && project.get("index") == agent.get("workspace")
            })
            .and_then(|project| project.get_mut("sessions"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        sessions.push(json!({
            "thread_id": null,
            "title": agent.get("surface_name"),
            "agent": agent.get("tool"),
            "pinned": false,
            "surface_id": surface_id,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::store::Device;
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

    fn phone(reply: bool) -> Device {
        let key = splitlane_remote::DeviceKey::generate().public();
        Device {
            reply,
            ..Device::new(splitlane_remote::RoomId::random(), "Pixel", &key, 0)
        }
    }

    fn ask(
        surface: &Recording,
        method: &str,
        params: Value,
        token: Option<&str>,
    ) -> Result<Value, String> {
        answer(surface, method, &params, &phone(true), token, true)
    }

    #[test]
    fn requests_become_the_clis_calls() {
        let surface = Recording::default();
        ask(&surface, "list", Value::Null, None).unwrap();
        ask(&surface, "status", json!({ "surface_id": 7 }), None).unwrap();
        ask(
            &surface,
            "read",
            json!({ "surface_id": 7, "lines": 9999 }),
            None,
        )
        .unwrap();
        let calls = surface.0.into_inner();
        assert_eq!(calls[0].0, "rail.list");
        assert_eq!(calls[1].0, "fleet.list");
        assert_eq!(calls[2].0, "workspace.list");
        assert_eq!(calls[3].0, "limits.list");
        assert_eq!(
            calls[4],
            ("surface.status".into(), json!({ "surface_id": 7 }))
        );
        assert_eq!(calls[5].0, "surface.read");
        assert_eq!(calls[5].1["lines"], READ_MAX_LINES);
        assert_eq!(calls[5].1["fenced"], false);
    }

    #[test]
    fn a_list_names_the_projects() {
        struct Window;
        impl IpcTransport for Window {
            fn call(&self, method: &str, _: Value) -> Result<Value, String> {
                match method {
                    "fleet.list" => Ok(json!({ "agents": [] })),
                    "workspace.list" => Ok(json!({ "workspaces": [
                        { "index": 0, "title": "splitlane", "cwd": "/home/me/splitlane", "panes": 3, "active": true }
                    ] })),
                    _ => Err("no".into()),
                }
            }
        }
        let list = answer(&Window, "list", &Value::Null, &phone(true), None, true).unwrap();
        assert_eq!(
            list["projects"],
            json!([{ "index": 0, "title": "splitlane" }])
        );
        // The session host names none, and the list is still the list.
        let closed = answer(
            &Recording::default(),
            "list",
            &Value::Null,
            &phone(true),
            None,
            false,
        );
        assert!(closed.is_ok());
    }

    #[test]
    fn a_list_carries_the_rails_list_with_every_running_agent_in_it() {
        struct Window;
        impl IpcTransport for Window {
            fn call(&self, method: &str, _: Value) -> Result<Value, String> {
                match method {
                    "fleet.list" => Ok(json!({ "agents": [
                        { "surface_id": 11, "surface_name": "claude@app", "tool": "claude", "workspace": 0 },
                        { "surface_id": 12, "surface_name": "codex@app", "tool": "codex", "workspace": 0 },
                        { "surface_id": 13, "surface_name": "claude@lost", "tool": "claude", "workspace": 5 }
                    ] })),
                    "workspace.list" => Ok(json!({ "workspaces": [] })),
                    "limits.list" => {
                        Ok(json!({ "vendors": [{ "vendor": "Claude", "windows": [] }] }))
                    }
                    "rail.list" => Ok(json!({
                        "groups": [{ "id": 1, "name": "Work", "private": false }],
                        "projects": [{ "index": 0, "title": "app", "group": 1, "sessions": [
                            { "thread_id": 3, "title": "Fix the login", "agent": "claude", "pinned": false, "surface_id": 11 },
                            { "thread_id": 2, "title": "Old idea", "agent": "claude", "pinned": false, "surface_id": null }
                        ] }]
                    })),
                    _ => Err("no".into()),
                }
            }
        }
        let list = answer(&Window, "list", &Value::Null, &phone(true), None, true).unwrap();
        let titles: Vec<&str> = list["rail"]["projects"][0]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["title"].as_str().unwrap())
            .collect();
        // The rail's names in the rail's order, the session at rest kept, and
        // the agent the rail has no row for after them under its pane name.
        assert_eq!(titles, ["Fix the login", "Old idea", "codex@app"]);
        assert_eq!(list["rail"]["groups"][0]["name"], "Work");
        assert_eq!(list["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(list["limits"]["vendors"][0]["vendor"], "Claude");
        // An agent in a project the rail does not list stays in `agents`.
        assert_eq!(list["agents"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_write_carries_the_token_and_the_phone() {
        let surface = Recording::default();
        ask(
            &surface,
            "send",
            json!({ "surface_id": 7, "text": "yes", "submit": true }),
            Some("t"),
        )
        .unwrap();
        ask(
            &surface,
            "key",
            json!({ "surface_id": 7, "keystroke": "escape" }),
            Some("t"),
        )
        .unwrap();
        ask(&surface, "interrupt", json!({ "surface_id": 7 }), Some("t")).unwrap();
        let calls = surface.0.into_inner();
        assert_eq!(calls[0].0, "surface.send_text");
        assert_eq!(calls[0].1["text"], "yes");
        assert_eq!(calls[0].1["submit"], true);
        assert_eq!(calls[0].1[crate::remote::spawn::TOKEN_PARAM], "t");
        assert_eq!(calls[0].1[crate::remote::spawn::DEVICE_PARAM], "Pixel");
        assert_eq!(calls[1].0, "surface.send_keystroke");
        assert_eq!(calls[2].0, "surface.interrupt");
    }

    #[test]
    fn a_write_without_the_windows_bridge_or_the_right_is_refused() {
        let surface = Recording::default();
        let send = json!({ "surface_id": 7, "text": "rm -rf ~", "submit": true });
        // A bridge started by hand has no token.
        assert!(ask(&surface, "send", send.clone(), None).is_err());
        // A phone paired to read only.
        assert!(answer(&surface, "send", &send, &phone(false), Some("t"), true).is_err());
        // Interrupting with the window closed.
        assert!(
            answer(
                &surface,
                "interrupt",
                &json!({ "surface_id": 7 }),
                &phone(true),
                Some("t"),
                false
            )
            .is_err()
        );
        // A key off the list.
        assert!(
            ask(
                &surface,
                "key",
                json!({ "surface_id": 7, "keystroke": "ctrl-d" }),
                Some("t")
            )
            .is_err()
        );
        assert!(
            surface.0.into_inner().is_empty(),
            "nothing reached the window"
        );
    }

    #[test]
    fn a_request_it_does_not_know_or_without_its_pane_is_refused() {
        let surface = Recording::default();
        assert!(ask(&surface, "close", json!({ "surface_id": 7 }), Some("t")).is_err());
        assert!(ask(&surface, "status", Value::Null, None).is_err());
        assert!(surface.0.into_inner().is_empty());
    }
}
