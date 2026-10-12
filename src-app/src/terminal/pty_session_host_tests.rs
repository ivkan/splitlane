//! Tests for a terminal whose PTY is in the session host.
//!
//! A child module of `pty_session`, so that it can look at a terminal's
//! grid and at the events its emulator emits. Nothing here asserts by
//! sleeping: every wait is a poll with a deadline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use splitlane_host::{
    AttachFrom, Client, ExitStatus, Facts, HostConfig, HostError, Rgb, SessionId, SessionInfo,
    SpawnSpec, StreamEvent,
};

use super::*;
use crate::terminal::host_session::{
    HostEndpoint, OpenError, Opened, child_environment, open, open_remote, reattach,
    start_and_follow,
};

/// A terminal as a pane builds one, with a sink feeding it, and everything
/// a pane would have switched on so that what is held back is held back by
/// the replay and by nothing else.
fn terminal_with_sink(mode: AttachMode, start: StreamStart) -> (TerminalState, RemoteSink) {
    let (mut state, pending) = TerminalState::new_pending(80, 24);
    let (sink, channels) = RemoteSink::new(&pending, mode, start);
    state.cwd_rx = Some(channels.cwd_rx);
    state.marks_rx = Some(channels.marks_rx);
    state.last_command = channels.last_command;
    state.set_osc52_mode(Osc52Mode::CopyPaste);
    state.set_terminal_focused(true);
    (state, sink)
}

fn start_at(start_seq: u64, tail_seq: u64) -> StreamStart {
    StreamStart {
        start_seq,
        tail_seq: Some(tail_seq),
        gap: false,
        cols: 80,
        rows: 24,
    }
}

fn output(seq: u64, bytes: &[u8]) -> StreamEvent {
    StreamEvent::Output {
        seq,
        bytes: bytes.to_vec(),
    }
}

/// Do what the view's event pump does, and say which of the emulator's
/// events came through.
fn pump(state: &mut TerminalState) -> Vec<&'static str> {
    let mut seen = Vec::new();
    let mut events = state.events_rx.take().expect("an event channel");
    while let Ok(event) = events.try_recv() {
        seen.push(match &event {
            AlacEvent::Wakeup => "wakeup",
            AlacEvent::PtyWrite(_) => "pty-write",
            AlacEvent::ColorRequest(..) => "color",
            AlacEvent::TextAreaSizeRequest(_) => "size",
            AlacEvent::ClipboardStore(..) => "store",
            AlacEvent::ClipboardLoad(..) => "load",
            AlacEvent::Bell => "bell",
            AlacEvent::Title(_) => "title",
            AlacEvent::ResetTitle => "reset-title",
            AlacEvent::ChildExit(_) => "child-exit",
            AlacEvent::Exit => "exit",
            _ => "other",
        });
        state.process_event(event);
    }
    state.events_rx = Some(events);
    state.sync();
    seen.retain(|name| *name != "other");
    seen
}

fn stored_to_clipboard(state: &mut TerminalState) -> Vec<String> {
    std::mem::take(&mut state.pending_clipboard_ops)
        .into_iter()
        .filter_map(|op| match op {
            ClipboardOp::Store(text) => Some(text),
            ClipboardOp::Load(_) => None,
        })
        .collect()
}

fn mark_count(state: &TerminalState) -> usize {
    state.marks.lock().expect("marks").iter().count()
}

fn screen(state: &TerminalState) -> String {
    let term = state.term.lock();
    let grid = term.grid();
    let mut out = String::new();
    for line in 0..grid.screen_lines() {
        let mut row = String::new();
        for col in 0..grid.columns() {
            row.push(grid[AlacPoint::new(GridLine(line as i32), GridCol(col))].c);
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// Everything a session can have done that a window acts on: a copy to the
/// clipboard, a working directory, a command with its marks, a title, a
/// bell, a dev server's address, and questions to the terminal. The
/// working directory it reports is `dir/word`.
fn a_busy_session_in(dir: &str, word: &str, port: u16) -> Vec<u8> {
    format!(
        "\x1b]0;{word}-title\x07\
         \x1b]7;file://localhost{dir}/{word}\x07\
         \x1b]133;A\x07$ \x1b]133;B\x07serve\r\n\
         \x1b]133;E;serve {word}\x07\x1b]133;C\x07\
         \x1b]52;c;{copied}\x07\
         \x07\
         \x1b[6n\x1b]11;?\x07\x1b[14t\x1b]52;c;?\x07\
         {word}: Local: http://localhost:{port}/\r\n\
         \x1b]133;D;0\x07",
        copied = match word {
            "old" => "b2xk",
            _ => "bmV3",
        },
    )
    .into_bytes()
}

fn a_busy_session(word: &str, port: u16) -> Vec<u8> {
    a_busy_session_in("/tmp", word, port)
}

#[test]
fn replaying_a_log_rebuilds_the_grid_and_acts_on_nothing() {
    let old = a_busy_session("old", 5173);
    let tail = old.len() as u64;
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Reattach, start_at(0, tail));

    // All but the last byte: still old output.
    let (most, last) = old.split_at(old.len() - 1);
    assert_eq!(sink.handle(output(0, most)), SinkStep::Continue);
    assert_eq!(pump(&mut state), Vec::<&str>::new());
    assert!(stored_to_clipboard(&mut state).is_empty());
    assert_eq!(mark_count(&state), 0);
    assert_eq!(state.title, "Terminal");
    assert_eq!(state.current_cwd, None);
    assert_eq!(state.output_generation, 0);
    // The grid is being rebuilt all the same.
    assert!(screen(&state).contains("old: Local: http://localhost:5173/"));

    // The last byte of the log: one title, one working directory, one
    // mark, and something to draw that is not counted as output.
    state.dirty = false;
    assert_eq!(
        sink.handle(output(tail - 1, last)),
        SinkStep::Continue,
        "nothing was lost, so nothing has to be repainted"
    );
    assert_eq!(pump(&mut state), ["title"]);
    assert_eq!(state.title, "old-title");
    assert_eq!(state.current_cwd.as_deref(), Some("/tmp/old"));
    assert_eq!(mark_count(&state), 1);
    assert_eq!(
        state.last_command.lock().expect("last command").as_deref(),
        Some("serve old")
    );
    assert!(stored_to_clipboard(&mut state).is_empty());
    assert_eq!(state.output_generation, 0);
    assert!(state.dirty);
    // The address on the screen was announced when it was printed.
    assert_eq!(state.scan_output(), Vec::new());

    // The same things happening now are acted on - except the questions,
    // which the host has answered. (The question for the clipboard's
    // content is one the emulator is built not to hear, in any pane.)
    let new = a_busy_session("new", 5174);
    assert_eq!(sink.handle(output(tail, &new)), SinkStep::Continue);
    let seen = pump(&mut state);
    assert_eq!(seen, ["title", "store", "bell", "wakeup"]);
    assert_eq!(state.title, "new-title");
    assert_eq!(state.current_cwd.as_deref(), Some("/tmp/new"));
    assert_eq!(mark_count(&state), 5);
    assert_eq!(stored_to_clipboard(&mut state), ["new"]);
    assert_eq!(state.output_generation, 1);
    assert_eq!(
        state
            .scan_output()
            .iter()
            .map(|service| service.port)
            .collect::<Vec<_>>(),
        [5174]
    );
    // Nothing waits to be answered from here: no size reply was queued.
    assert!(
        state
            .drain_size_responses(TerminalWindowSize::new(80, 24, 9, 18))
            .is_empty()
    );
}

#[test]
fn a_chunk_across_the_end_of_the_log_is_old_up_to_it_and_new_after() {
    let old = b"\x1b]52;c;b2xk\x07before ";
    let new = b"\x1b]52;c;bmV3\x07after";
    let mut chunk = old.to_vec();
    chunk.extend_from_slice(new);
    // The stream starts in the middle of the session, as after a detach.
    let start = 1000;
    let (mut state, mut sink) = terminal_with_sink(
        AttachMode::Reattach,
        start_at(start, start + old.len() as u64),
    );

    assert_eq!(sink.handle(output(start, &chunk)), SinkStep::Continue);
    assert_eq!(pump(&mut state), ["store", "wakeup"]);
    assert_eq!(stored_to_clipboard(&mut state), ["new"]);
    assert_eq!(state.output_generation, 1);
    assert!(screen(&state).contains("before after"));
}

#[test]
fn the_log_is_replayed_at_the_sizes_it_recorded() {
    let start = StreamStart {
        cols: 40,
        rows: 10,
        ..start_at(0, 9)
    };
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Reattach, start);
    let backend = state.session_backend();
    assert_eq!(backend.grid_size(), (40, 10));
    // A frame drawn in the middle of the replay wants the window's size.
    // It gets the grid as it is.
    let window = TerminalWindowSize::new(120, 30, 9, 18);
    let (content, _) = backend.render_content(window, 0, 30, false);
    assert_eq!((content.cols, content.rows), (40, 10));

    assert_eq!(sink.handle(output(0, b"abcd")), SinkStep::Continue);
    let resized = |seq, cols, rows| StreamEvent::Resized { seq, cols, rows };
    assert_eq!(sink.handle(resized(4, 60, 20)), SinkStep::Continue);
    assert_eq!(backend.grid_size(), (60, 20));
    assert_eq!(sink.handle(output(5, b"efgh")), SinkStep::Continue);
    pump(&mut state);
    assert_eq!(state.output_generation, 0);

    // Live, the window decides: a resize in the stream is the echo of its
    // own request, and the next frame applies its size.
    assert_eq!(sink.handle(resized(9, 70, 21)), SinkStep::Continue);
    assert_eq!(backend.grid_size(), (60, 20));
    let (content, _) = backend.render_content(window, 0, 30, false);
    assert_eq!((content.cols, content.rows), (120, 30));
}

#[test]
fn a_session_with_nothing_to_replay_is_live_at_once() {
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Reattach, start_at(42, 42));
    state.dirty = false;
    pump(&mut state);
    assert!(state.dirty, "the terminal is told once that it may draw");
    assert_eq!(sink.handle(output(42, b"\x07hello")), SinkStep::Continue);
    assert_eq!(pump(&mut state), ["bell", "wakeup"]);

    // A host that does not say where its log ended: nothing is old.
    let start = StreamStart {
        tail_seq: None,
        ..start_at(0, 0)
    };
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Reattach, start);
    assert_eq!(sink.handle(output(0, b"\x07")), SinkStep::Continue);
    assert_eq!(pump(&mut state), ["bell", "wakeup"]);
}

#[test]
fn a_session_the_window_started_is_live_from_its_first_byte() {
    // The log already holds the first prompt when the attach is answered.
    let prompt = a_busy_session("first", 3000);
    let (mut state, mut sink) =
        terminal_with_sink(AttachMode::Fresh, start_at(0, prompt.len() as u64));
    assert_eq!(sink.handle(output(0, &prompt)), SinkStep::Continue);
    assert_eq!(pump(&mut state), ["title", "store", "bell", "wakeup"]);
    assert_eq!(state.title, "first-title");
    assert_eq!(state.current_cwd.as_deref(), Some("/tmp/first"));
    assert_eq!(mark_count(&state), 4);

    // And its grid is the window's: the host's record of a resize is not
    // an instruction.
    let step = sink.handle(StreamEvent::Resized {
        seq: prompt.len() as u64,
        cols: 10,
        rows: 5,
    });
    assert_eq!(step, SinkStep::Continue);
    assert_eq!(state.session_backend().grid_size(), (80, 24));
}

#[test]
fn a_log_that_lost_its_start_restores_the_modes_and_asks_for_one_repaint() {
    let start = StreamStart {
        gap: true,
        ..start_at(500, 510)
    };
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Reattach, start);
    // The modes in effect where the log now begins: the alternate screen
    // and a title.
    let preamble = b"\x1b[?1049h\x1b]0;editor\x07".to_vec();
    assert_eq!(
        sink.handle(StreamEvent::Preamble(preamble)),
        SinkStep::Continue
    );
    assert_eq!(pump(&mut state), Vec::<&str>::new());
    assert!(state.session_backend().modes().contains(Modes::ALT_SCREEN));

    // The repaint is asked for once, when the replay is over.
    assert_eq!(sink.handle(output(500, b"0123")), SinkStep::Continue);
    assert_eq!(sink.handle(output(504, b"456789")), SinkStep::Redraw);
    assert_eq!(pump(&mut state), ["title"]);
    assert_eq!(state.title, "editor");
    assert_eq!(state.output_generation, 0);

    // Falling behind later is the same loss, live: the modes at once,
    // then the repaint.
    let gap = StreamEvent::Gap {
        lost_from: 510,
        resumes_at: 9000,
    };
    assert_eq!(sink.handle(gap), SinkStep::Continue);
    assert_eq!(
        sink.handle(StreamEvent::Preamble(b"\x1b[?1049l\x1b[6n".to_vec())),
        SinkStep::Redraw
    );
    assert_eq!(pump(&mut state), ["wakeup"]);
    assert!(!state.session_backend().modes().contains(Modes::ALT_SCREEN));
    assert_eq!(sink.handle(output(9000, b"x")), SinkStep::Continue);
}

#[test]
fn a_log_with_nothing_left_but_its_modes_ends_its_replay_at_the_preamble() {
    let start = StreamStart {
        gap: true,
        ..start_at(700, 700)
    };
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Reattach, start);
    assert_eq!(pump(&mut state), Vec::<&str>::new());
    assert_eq!(
        sink.handle(StreamEvent::Preamble(b"\x1b]0;shell\x07".to_vec())),
        SinkStep::Redraw
    );
    assert_eq!(pump(&mut state), ["title"]);
    assert_eq!(sink.handle(output(700, b"\x07")), SinkStep::Continue);
    assert_eq!(pump(&mut state), ["bell", "wakeup"]);
}

#[test]
fn a_gap_that_passes_the_end_of_the_log_ends_the_replay() {
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Reattach, start_at(0, 100));
    assert_eq!(
        sink.handle(output(0, b"\x1b]0;early\x07")),
        SinkStep::Continue
    );
    let gap = StreamEvent::Gap {
        lost_from: 10,
        resumes_at: 4000,
    };
    assert_eq!(sink.handle(gap), SinkStep::Continue);
    assert_eq!(pump(&mut state), ["title"]);
    assert_eq!(
        sink.handle(StreamEvent::Preamble(b"\x1b[?25h".to_vec())),
        SinkStep::Redraw
    );
    assert_eq!(pump(&mut state), ["wakeup"]);
    assert_eq!(sink.handle(output(4000, b"\x07")), SinkStep::Continue);
    assert_eq!(pump(&mut state), ["bell", "wakeup"]);
}

#[test]
fn a_synchronized_update_is_drawn_when_it_ends_or_when_its_time_is_up() {
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Fresh, start_at(0, 0));
    assert_eq!(sink.sync_deadline(), None);
    // Beginning one is not itself held back.
    sink.handle(output(0, b"\x1b[?2026h"));
    assert_eq!(pump(&mut state), ["wakeup"]);
    assert!(sink.sync_deadline().is_some());
    sink.handle(output(8, b"half a frame"));
    assert_eq!(pump(&mut state), Vec::<&str>::new());
    assert!(!screen(&state).contains("half a frame"));
    sink.handle(output(20, b" and the rest\x1b[?2026l"));
    assert_eq!(pump(&mut state), ["wakeup"]);
    assert!(screen(&state).contains("half a frame and the rest"));
    assert_eq!(sink.sync_deadline(), None);

    // One the program never ends is shown when the emulator's patience
    // runs out, and not before.
    sink.handle(output(41, b"\x1b[?2026h"));
    pump(&mut state);
    sink.handle(output(49, b" unfinished"));
    sink.quiet();
    assert_eq!(pump(&mut state), Vec::<&str>::new());
    assert!(!screen(&state).contains("unfinished"));
    let deadline = sink.sync_deadline().expect("an update is being held");
    let give_up = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        assert!(std::time::Instant::now() < give_up);
        std::thread::yield_now();
    }
    sink.quiet();
    assert_eq!(pump(&mut state), ["wakeup"]);
    assert!(screen(&state).contains("unfinished"));
}

#[test]
fn the_end_of_a_session_reaches_the_terminal_as_the_end_of_a_child() {
    use std::os::unix::process::ExitStatusExt;

    let code = |code| ExitStatus {
        code: Some(code),
        signal: None,
    };
    let signal = |signal| ExitStatus {
        code: None,
        signal: Some(signal),
    };
    let translated = |status| exit_status_from_host(status).expect("a status");
    assert_eq!(translated(code(0)).code(), Some(0));
    assert!(translated(code(0)).success());
    assert_eq!(translated(code(3)).code(), Some(3));
    assert_eq!(translated(code(255)).code(), Some(255));
    assert_eq!(translated(signal(9)).code(), None);
    assert_eq!(translated(signal(9)).signal(), Some(9));
    assert_eq!(translated(signal(15)).signal(), Some(15));
    assert!(exit_status_from_host(ExitStatus::default()).is_none());

    // Output printed just before the exit is on the screen when the exit
    // is known.
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Fresh, start_at(0, 0));
    sink.handle(output(0, b"last words\r\n"));
    assert_eq!(sink.handle(StreamEvent::Exited(code(3))), SinkStep::Ended);
    assert_eq!(pump(&mut state), ["wakeup", "child-exit", "exit", "wakeup"]);
    assert_eq!(state.exited, Some(3));
    assert_eq!(state.exit_signal, None);
    assert!(screen(&state).contains("last words"));

    let (mut state, mut sink) = terminal_with_sink(AttachMode::Fresh, start_at(0, 0));
    assert_eq!(
        sink.handle(StreamEvent::Exited(signal(15))),
        SinkStep::Ended
    );
    pump(&mut state);
    assert_eq!(state.exited, Some(-1));
    assert!(
        state
            .exit_signal
            .as_deref()
            .is_some_and(|text| text.starts_with("15 (")),
        "{:?}",
        state.exit_signal
    );

    // A host that cannot say how the child ended still says that it did.
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Fresh, start_at(0, 0));
    assert_eq!(
        sink.handle(StreamEvent::Exited(ExitStatus::default())),
        SinkStep::Ended
    );
    assert_eq!(pump(&mut state), ["exit", "wakeup"]);
    assert_eq!(state.exited, Some(-1));

    // A session that had ended before the window attached: its log, and
    // then its end, with nothing acted on in between.
    let old = a_busy_session("old", 5173);
    let (mut state, mut sink) =
        terminal_with_sink(AttachMode::Reattach, start_at(0, old.len() as u64 + 50));
    sink.handle(output(0, &old));
    assert_eq!(sink.handle(StreamEvent::Exited(code(0))), SinkStep::Ended);
    assert_eq!(pump(&mut state), ["title", "child-exit", "exit", "wakeup"]);
    assert_eq!(state.exited, Some(0));
    assert!(stored_to_clipboard(&mut state).is_empty());
}

#[test]
fn a_lost_connection_ends_the_pane_and_says_why() {
    let (mut state, mut sink) = terminal_with_sink(AttachMode::Fresh, start_at(0, 0));
    sink.handle(output(0, b"working"));
    sink.connection_lost();
    assert_eq!(pump(&mut state), ["wakeup", "exit", "wakeup"]);
    assert_eq!(state.exited, Some(-1));
    let screen = screen(&state);
    assert!(screen.contains("working"), "{screen}");
    assert!(
        screen.contains("The connection to the session host was lost."),
        "{screen}"
    );
}

// ---------------------------------------------------------------------------
// From here on: a real host on a temporary socket, real children.
// ---------------------------------------------------------------------------

const DEADLINE: Duration = Duration::from_secs(20);

/// Poll `probe` until it yields something, or fail naming `what`.
fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
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
struct TestHost {
    // Field order matters: the directory is removed last.
    thread: Option<JoinHandle<Result<(), HostError>>>,
    endpoint: Arc<HostEndpoint>,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// A directory short enough for a socket path: macOS allows 104 bytes and
/// its default temporary directory uses half of them.
fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("slp")
        .tempdir_in("/tmp")
        .expect("a temporary directory")
}

impl TestHost {
    fn start(configure: impl FnOnce(&mut HostConfig)) -> Self {
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

    fn client(&self) -> Client {
        wait_for("the host to answer", || {
            assert!(
                !self.thread.as_ref().is_some_and(JoinHandle::is_finished),
                "the host exited before answering"
            );
            Client::connect(&self.socket).ok()
        })
    }

    fn sessions(&self) -> Vec<SessionInfo> {
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
fn pane_params(program: &str, args: &[&str]) -> SpawnParams {
    let mut params =
        TerminalState::resolve_spawn_params(Some(PathBuf::from("/")), 7, 11, Some((80, 24)), None);
    params.shell = program.to_string();
    params.extra_args = args.iter().map(|arg| arg.to_string()).collect();
    params
}

fn sh(script: &str) -> SpawnParams {
    pane_params("/bin/sh", &["-c", script])
}

fn facts() -> Facts {
    window_facts(&crate::theme::active_theme(), None, None)
}

/// A pane opened in the host, the way the view opens one.
fn pane_in_host(host: &TestHost, params: SpawnParams) -> TerminalState {
    let (mut state, pending) = TerminalState::new_pending(params.cols, params.rows);
    match open(Some(&host.endpoint), facts(), params, pending, None) {
        Opened::InHost(spawned) => state.promote_remote(spawned),
        Opened::InWindow(_) => unreachable!("the host did not take the pane"),
        Opened::Lost(e) => unreachable!("the pane lost its shell: {e:#}"),
    }
    state.set_terminal_focused(true);
    state
}

/// A pane with a PTY the window owns, for comparison.
fn pane_in_window(params: SpawnParams) -> TerminalState {
    let (mut state, pending) = TerminalState::new_pending(params.cols, params.rows);
    let spawned = TerminalState::open_pty_and_eventloop(params, pending, None)
        .expect("a PTY the window owns");
    state.promote(spawned);
    state
}

/// Everything the pane holds as text, the history and the screen.
fn text(state: &TerminalState) -> String {
    state.extract_text().unwrap_or_default()
}

/// Pump the pane until its text contains `needle`.
fn wait_for_text(state: &mut TerminalState, needle: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        state.sync();
        if text(state).contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{needle:?} never appeared in:\n{}",
            text(state)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_exit(state: &mut TerminalState) -> i32 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        state.sync();
        if let Some(code) = state.exited {
            return code;
        }
        assert!(
            Instant::now() < deadline,
            "the pane never saw its child end:\n{}",
            text(state)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn group_alive(pid: u32) -> bool {
    // SAFETY: signal 0 to a process group only asks whether it has members.
    unsafe { libc::kill(-(pid as libc::pid_t), 0) == 0 }
}

fn session_of(state: &TerminalState) -> SessionId {
    state.remote.as_ref().expect("a pane in the host").session()
}

#[test]
fn a_pane_in_the_host_runs_a_shell_the_host_started() {
    let host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, pane_params("/bin/sh", &[]));
    assert!(pane.is_in_host());
    assert!(pane.notifier.0.is_live());

    // The child is the host's, a session leader as a pane's child always
    // is: the port scan and the teardown both rely on its pid naming its
    // group.
    let pid = pane.child_pid;
    assert!(pid > 1);
    // SAFETY: getpgid only queries.
    assert_eq!(
        unsafe { libc::getpgid(pid as libc::pid_t) },
        pid as libc::pid_t
    );
    let listed = host.sessions();
    let [session] = listed.as_slice() else {
        unreachable!("one session expected, the host lists {listed:?}");
    };
    assert_eq!(session.id, session_of(&pane));
    assert_eq!(session.pid, pid);
    assert_eq!(session.exited, None);
    assert_eq!((session.cols, session.rows), (80, 24));
    assert_eq!(session.cwd, "/");
    // Which pane of which window the session belongs to.
    assert_eq!(
        session.exported.get("surface_id").map(String::as_str),
        Some("11")
    );
    assert_eq!(
        session.exported.get("workspace_id").map(String::as_str),
        Some("7")
    );
    assert_eq!(
        session.exported.get("window_pid"),
        Some(&std::process::id().to_string())
    );
    assert_eq!(pane.current_cwd.as_deref(), Some("/"));

    pane.write_to_pty(b"echo SPLITLANE_$((40 + 2))_OK\n".to_vec());
    wait_for_text(&mut pane, "SPLITLANE_42_OK");
    assert!(pane.output_generation > 0);
    assert_eq!(pane.refused_input_count(), 0);
}

#[test]
fn input_written_before_the_shell_exists_reaches_it() {
    let host = TestHost::start(|_| {});
    let params = pane_params("/bin/sh", &[]);
    let (mut pane, pending) = TerminalState::new_pending(params.cols, params.rows);
    // An agent's launch command is written the moment its pane mounts.
    pane.write_to_pty(b"echo EARLY_$((1 + 1))\n".to_vec());
    let spawned = match open_remote(&params, &pending, &host.endpoint, facts()) {
        Ok(spawned) => spawned,
        Err(OpenError::BeforeSpawn(e) | OpenError::AfterSpawn(e)) => {
            unreachable!("the pane was not opened in the host: {e:#}")
        }
    };
    pane.promote_remote(spawned);
    wait_for_text(&mut pane, "EARLY_2");
}

/// A shell must not be able to tell from its environment where its PTY
/// lives. `env` itself is the program, because a shell in between adds
/// variables of its own.
#[test]
fn a_shell_in_the_host_gets_the_environment_a_shell_in_the_window_gets() {
    let host = TestHost::start(|_| {});
    // Wide, so that no value is wrapped over two rows.
    let wide = || {
        let mut params = pane_params("/usr/bin/env", &[]);
        params.cols = 4096;
        params
    };
    let environment = |pane: &mut TerminalState| {
        assert_eq!(wait_for_exit(pane), 0);
        let mut lines: Vec<String> = text(pane).lines().map(str::to_owned).collect();
        lines.sort_unstable();
        lines
    };
    let in_window = environment(&mut pane_in_window(wide()));
    let in_host = environment(&mut pane_in_host(&host, wide()));

    assert!(in_host.len() > 10, "{in_host:?}");
    for expected in [
        "TERM=xterm-256color",
        "TERM_PROGRAM=splitlane",
        "COLORTERM=truecolor",
        "SHLVL=0",
        "SPLITLANE_SURFACE_ID=11",
        "SPLITLANE_WORKSPACE_ID=7",
        "ALACRITTY_WINDOW_ID=11",
        "WINDOWID=11",
    ] {
        assert!(in_host.iter().any(|line| line == expected), "{expected}");
    }
    assert!(in_host.iter().any(|line| line.starts_with("HOME=/")));
    assert!(in_host.iter().any(|line| line.starts_with("USER=")));
    // No variable is exempt: nothing differs legitimately.
    assert_eq!(in_host, in_window);
}

#[test]
fn the_environment_sent_to_the_host_is_the_one_the_emulator_would_build() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let inherited = vec![
        (OsString::from("PATH"), OsString::from("/usr/bin")),
        (OsString::from("USER"), OsString::from("somebody")),
        (OsString::from("HOME"), OsString::from("/home/somebody")),
        (OsString::from("TERM"), OsString::from("dumb")),
        (OsString::from("WINDOWID"), OsString::from("999")),
        (OsString::from("XDG_ACTIVATION_TOKEN"), OsString::from("t")),
        // Not text: left out, where reading the environment the easy way
        // would have stopped the pane from opening at all.
        (
            OsString::from("BINARY"),
            OsString::from_vec(vec![0xff, 0xfe]),
        ),
    ];
    let pane = HashMap::from([
        ("TERM".to_string(), "xterm-256color".to_string()),
        (
            "DESKTOP_STARTUP_ID".to_string(),
            "from the pane".to_string(),
        ),
    ]);
    let env = child_environment(inherited.into_iter(), &pane, 42).expect("an environment");
    let expected = [
        ("ALACRITTY_WINDOW_ID", "42"),
        ("HOME", "/home/somebody"),
        ("PATH", "/usr/bin"),
        // The pane's variables win over what was inherited.
        ("TERM", "xterm-256color"),
        ("USER", "somebody"),
        // The emulator's own win over what was inherited too.
        ("WINDOWID", "42"),
    ];
    assert_eq!(
        env,
        expected.map(|(key, value)| (key.to_string(), value.to_string()))
    );

    // A window started with neither variable still gives its shell both.
    let bare = child_environment(std::iter::empty(), &HashMap::new(), 1).expect("an environment");
    assert!(
        bare.iter()
            .any(|(key, value)| key == "HOME" && value.starts_with('/'))
    );
    assert!(
        bare.iter()
            .any(|(key, value)| key == "USER" && !value.is_empty())
    );
}

#[test]
fn a_resize_reaches_the_program_and_the_pane_knows_when_it_has() {
    let host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, pane_params("/bin/sh", &[]));
    // What the host was asked to start with is what the PTY has.
    assert_eq!(pane.kernel_window_size(), Some((80, 24)));
    pane.write_to_pty(b"stty size\n".to_vec());
    wait_for_text(&mut pane, "24 80");

    pane.notify_window_size(TerminalWindowSize::new(100, 30, 9, 18));
    wait_for("the host to confirm the size", || {
        (pane.kernel_window_size() == Some((100, 30))).then_some(())
    });
    assert_eq!(
        host.sessions().first().map(|info| (info.cols, info.rows)),
        Some((100, 30))
    );
    pane.write_to_pty(b"stty size\n".to_vec());
    wait_for_text(&mut pane, "30 100");
}

/// What a font zoom is to the PTY: the same grid with cells of another
/// size. The program is signalled about it, in the host as in a pane the
/// window owns, where the same call sets the same fields.
#[test]
fn a_change_of_cell_size_alone_reaches_the_program_as_a_resize() {
    let host = TestHost::start(|_| {});
    let counting =
        || sh("trap 'echo resized-$((n += 1))' WINCH; echo ready; while :; do sleep 0.02; done");
    for mut pane in [pane_in_host(&host, counting()), pane_in_window(counting())] {
        wait_for_text(&mut pane, "ready");
        pane.notify_window_size(TerminalWindowSize::new(80, 24, 9, 18));
        wait_for_text(&mut pane, "resized-1");
        pane.notify_window_size(TerminalWindowSize::new(80, 24, 11, 22));
        wait_for_text(&mut pane, "resized-2");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A shell command that reads exactly `count` bytes of input and prints
/// them as hexadecimal digits. The terminal must be in raw mode.
fn read_hex(count: usize) -> String {
    format!("dd bs=1 count={count} 2>/dev/null | od -An -tx1 | tr -d ' \\n'")
}

/// A shell command that prints whatever input arrives within a third of a
/// second, as hexadecimal digits between brackets: empty brackets when the
/// terminal had nothing more to say.
const READ_STRAGGLERS: &str = "stty min 0 time 3; printf ' more[%s]' \
     \"$(dd bs=64 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n')\"; stty min 1 time 0";

/// The pane's text with the rows joined, for output longer than a row.
fn on_one_line(state: &TerminalState) -> String {
    text(state).replace('\n', "")
}

fn color_answer(code: u8, color: Rgb) -> Vec<u8> {
    let Rgb(r, g, b) = color;
    format!("\x1b]{code};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x07").into_bytes()
}

#[test]
fn a_program_is_answered_once_and_by_the_host() {
    let host = TestHost::start(|_| {});
    let background = facts().background.expect("a theme has a background");
    let mut first = b"\x1b[1;1R".to_vec();
    first.extend(color_answer(11, background));
    let switched = Rgb(0x12, 0x34, 0x56);
    let second = color_answer(11, switched);
    // Two rounds, the second started by a byte of input: where is the
    // cursor and what is the background, then the background again after
    // the window changed its theme.
    let script = format!(
        "stty raw -echo; printf '\\033[6n\\033]11;?\\007'; {}; {READ_STRAGGLERS}; printf ' first;'; \
         dd bs=1 count=1 2>/dev/null >/dev/null; printf '\\033]11;?\\007'; {}; {READ_STRAGGLERS}; \
         printf ' second;'; dd bs=1 count=1 2>/dev/null >/dev/null",
        read_hex(first.len()),
        read_hex(second.len()),
    );
    let mut pane = pane_in_host(&host, sh(&script));

    // Everything the pane's own emulator emits while the program asks.
    let mut emitted = Vec::new();
    let mut pump_until = |pane: &mut TerminalState, needle: &str| {
        let deadline = Instant::now() + DEADLINE;
        while !text(pane).contains(needle) {
            emitted.extend(pump(pane));
            assert!(Instant::now() < deadline, "{needle:?}: {}", text(pane));
            std::thread::sleep(Duration::from_millis(5));
        }
        emitted.extend(pump(pane));
    };
    pump_until(&mut pane, " first;");
    // Exactly the two answers, and nothing behind them: a second answer
    // from the window would be between the brackets.
    assert!(
        on_one_line(&pane).contains(&format!("{} more[] first;", hex(&first))),
        "{}",
        text(&pane)
    );

    pane.tell_host(Facts {
        background: Some(switched),
        ..facts()
    });
    // Facts and input travel in the order they were said.
    pane.write_to_pty(b"g".to_vec());
    pump_until(&mut pane, " second;");
    assert!(
        on_one_line(&pane).contains(&format!("{} more[] second;", hex(&second))),
        "{}",
        text(&pane)
    );

    for held_back in ["pty-write", "color", "size"] {
        assert!(!emitted.contains(&held_back), "{emitted:?}");
    }
    assert!(emitted.contains(&"wakeup"));
    assert!(
        pane.drain_size_responses(TerminalWindowSize::new(80, 24, 9, 18))
            .is_empty()
    );
    assert_eq!(host.sessions()[0].dropped_replies, 0);
}

#[test]
fn how_a_child_ended_reaches_the_pane_after_its_last_output() {
    let host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, sh("printf 'last words'; exit 3"));
    assert_eq!(wait_for_exit(&mut pane), 3);
    assert_eq!(pane.exit_signal, None);
    assert!(text(&pane).contains("last words"), "{}", text(&pane));
    // An exit with a code and no input from anybody: a pane that stays
    // open to show the code, as for a shell that failed to start.
    assert!(!pane.should_close_on_exit());

    let mut killed = pane_in_host(&host, sh("printf 'about to go'; kill -TERM $$"));
    assert_eq!(wait_for_exit(&mut killed), -1);
    assert!(
        killed
            .exit_signal
            .as_deref()
            .is_some_and(|signal| signal.starts_with("15 (")),
        "{:?}",
        killed.exit_signal
    );
    assert!(text(&killed).contains("about to go"));

    // A session that ended by itself is not left in the host.
    wait_for("the host to forget both sessions", || {
        host.sessions().is_empty().then_some(())
    });
}

#[test]
fn closing_a_pane_ends_its_session_and_everything_in_it() {
    let host = TestHost::start(|_| {});
    // A shell with a child of its own, which is what a pane usually holds.
    let mut pane = pane_in_host(&host, sh("sleep 300 & echo started; wait"));
    wait_for_text(&mut pane, "started");
    let pid = pane.child_pid;
    assert!(group_alive(pid));

    drop(pane);
    wait_for("the process group to be gone", || {
        (!group_alive(pid)).then_some(())
    });
    wait_for("the host to forget the session", || {
        host.sessions().is_empty().then_some(())
    });
}

/// Input a program does not read is not taken off the pane's connection by
/// the host, and everything the pane says after it waits behind it. Ending
/// the session must not be one of the things that wait.
#[test]
fn closing_a_pane_does_not_wait_for_a_program_that_reads_nothing() {
    let host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, sh("stty raw -echo; echo deaf; sleep 300"));
    wait_for_text(&mut pane, "deaf");
    let pid = pane.child_pid;

    // Until the terminal, the host's queue for it and the socket hold no
    // more: then the connection is held up, the pane's own queue stops
    // emptying, and at its limit the pane says no.
    let chunk = vec![b'x'; 4 * 1024];
    wait_for("input to be refused", || {
        pane.write_to_pty(chunk.clone());
        (pane.refused_input_count() > 0).then_some(())
    });
    // A resize said now is behind all of that, and is not confirmed.
    pane.notify_window_size(TerminalWindowSize::new(100, 30, 9, 18));
    assert_eq!(pane.kernel_window_size(), Some((80, 24)));

    let closing = Instant::now();
    drop(pane);
    let took = closing.elapsed();
    assert!(
        took < Duration::from_secs(1),
        "closing the pane took {took:?}"
    );
    wait_for("the process group to be gone", || {
        (!group_alive(pid)).then_some(())
    });
    wait_for("the host to forget the session", || {
        host.sessions().is_empty().then_some(())
    });
}

/// A shell script that prints `bytes` exactly.
fn print_bytes(bytes: &[u8]) -> String {
    let octal: String = bytes.iter().map(|byte| format!("\\{byte:03o}")).collect();
    format!("printf '{octal}'")
}

fn log_tail(host: &TestHost, session: SessionId) -> u64 {
    host.sessions()
        .into_iter()
        .find(|info| info.id == session)
        .map_or(0, |info| info.log_tail_seq)
}

/// A terminal attached to a session that was already running.
fn reattached(host: &TestHost, session: SessionId) -> TerminalState {
    let (mut state, pending) = TerminalState::new_pending(80, 24);
    state.set_terminal_focused(true);
    let spawned = reattach(&host.endpoint, session, &pending).expect("the session can be attached");
    state.promote_remote(spawned);
    state
}

#[test]
fn a_terminal_attached_to_a_running_session_replays_it_quietly_and_then_follows_it() {
    let host = TestHost::start(|_| {});
    // The directories the session says it is in are the ones it is in: a
    // pane also asks the process table now and then, and the two answers
    // have to agree for the test to read one of them.
    let dirs = short_dir();
    let dir = std::fs::canonicalize(dirs.path()).expect("a real path");
    let dir = dir.to_str().expect("a path that is text");
    for word in ["old", "new"] {
        std::fs::create_dir(format!("{dir}/{word}")).expect("a directory");
    }
    let old = a_busy_session_in(dir, "old", 5173);
    let new = a_busy_session_in(dir, "new", 5174);
    // The session runs with nobody looking, then waits for a `Z`: what it
    // reads before that are the host's answers to its own questions.
    let script = format!(
        "stty raw -echo; cd {dir}/old; {}; \
         while [ \"$(dd bs=1 count=1 2>/dev/null)\" != Z ]; do :; done; \
         cd {dir}/new; {}; sleep 300",
        print_bytes(&old),
        print_bytes(&new)
    );
    let control = host.client();
    let (session, pid) = control
        .spawn(SpawnSpec {
            argv: vec!["/bin/sh".into(), "-c".into(), script],
            env: vec![("PATH".into(), "/bin:/usr/bin".into())],
            cwd: "/".into(),
            cols: 80,
            rows: 24,
            facts: Some(facts()),
            ..SpawnSpec::default()
        })
        .expect("a session");
    wait_for("the session to have printed", || {
        (log_tail(&host, session) == old.len() as u64).then_some(())
    });

    let mut pane = reattached(&host, session);
    assert_eq!(pane.child_pid, pid);
    // The replay is over when the terminal has been told so, which is the
    // last thing the replay does; one more round then collects whatever
    // was sent just ahead of that.
    let mut seen = Vec::new();
    wait_for("the replay to end", || {
        seen.extend(pump(&mut pane));
        pane.last_activity_burst.map(|_| ())
    });
    seen.extend(pump(&mut pane));
    assert_eq!(seen, ["title"]);
    assert_eq!(pane.title, "old-title");
    assert_eq!(pane.current_cwd, Some(format!("{dir}/old")));
    assert!(stored_to_clipboard(&mut pane).is_empty());
    assert_eq!(pane.output_generation, 0);
    // Of the marks only the last, placed where the replay left the cursor.
    assert_eq!(mark_count(&pane), 1);
    assert!(pane.dirty);
    assert!(text(&pane).contains("old: Local: http://localhost:5173/"));
    assert_eq!(pane.scan_output(), Vec::new());

    // Now the session goes on, with the pane watching.
    pane.write_to_pty(b"Z".to_vec());
    wait_for("the new output", || {
        seen.extend(pump(&mut pane));
        text(&pane)
            .contains("new: Local: http://localhost:5174/")
            .then_some(())
    });
    wait_for("the end of the new output", || {
        seen.extend(pump(&mut pane));
        (mark_count(&pane) == 5).then_some(())
    });
    assert_eq!(pane.current_cwd, Some(format!("{dir}/new")));
    assert_eq!(pane.title, "new-title");
    assert_eq!(stored_to_clipboard(&mut pane), ["new"]);
    assert!(pane.output_generation > 0);
    assert_eq!(
        pane.scan_output()
            .iter()
            .map(|service| service.port)
            .collect::<Vec<_>>(),
        [5174]
    );
    assert_eq!(seen.iter().filter(|name| **name == "bell").count(), 1);
    for held_back in ["pty-write", "color", "size"] {
        assert!(!seen.contains(&held_back), "{seen:?}");
    }
    // Asked once by the old output and once by the new, answered once
    // each, by the host: nothing came from the window.
    assert_eq!(host.sessions()[0].dropped_replies, 0);
}

#[test]
fn a_terminal_attached_to_a_cut_log_gets_the_modes_and_asks_for_one_repaint() {
    let host = TestHost::start(|config| config.log_capacity_bytes = 4096);
    // A full-screen program that has printed far more than the log holds,
    // and counts how often it is told to repaint.
    let script = "printf '\\033[?1049h\\033]2;editor\\007'; \
         i=0; while [ $i -lt 2000 ]; do echo \"line $i\"; i=$((i + 1)); done; \
         trap 'echo repaint-$((n += 1))' WINCH; echo painted; \
         while :; do sleep 0.02; done";
    let control = host.client();
    let (session, _) = control
        .spawn(SpawnSpec {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: vec![("PATH".into(), "/bin:/usr/bin".into())],
            cwd: "/".into(),
            cols: 80,
            rows: 24,
            ..SpawnSpec::default()
        })
        .expect("a session");
    // Followed from the start by a client that keeps up, to know when the
    // program is ready and to see every resize the host records.
    let watcher = control
        .attach(session, AttachFrom::Start)
        .expect("a stream");
    let mut printed = String::new();
    let mut resizes: Vec<(u16, u16)> = Vec::new();
    type Seen<'a> = &'a dyn Fn(&str, &[(u16, u16)]) -> bool;
    let mut watch_until = |what: &str, done: Seen<'_>| {
        let deadline = Instant::now() + DEADLINE;
        while !done(&printed, &resizes) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; resizes so far: {resizes:?}"
            );
            match watcher.recv_timeout(Duration::from_millis(50)) {
                Ok(StreamEvent::Output { bytes, .. }) => {
                    printed.push_str(&String::from_utf8_lossy(&bytes));
                }
                Ok(StreamEvent::Resized { cols, rows, .. }) => resizes.push((cols, rows)),
                _ => {}
            }
        }
        resizes.clone()
    };
    watch_until("the program to have painted", &|printed, _| {
        printed.contains("painted")
    });
    let listed = host.sessions();
    assert!(listed[0].log_head_seq > 0, "{listed:?}");

    let mut pane = reattached(&host, session);
    wait_for("the replay to end", || {
        pump(&mut pane);
        pane.last_activity_burst.map(|_| ())
    });
    assert_eq!(pane.title, "editor");
    assert!(pane.session_backend().modes().contains(Modes::ALT_SCREEN));

    // One repaint is one row less and back, and the program is told.
    watch_until("the repaint", &|printed, resizes| {
        resizes.len() >= 2 && printed.contains("repaint-1")
    });
    wait_for_text(&mut pane, "repaint-1");
    // A resize of the pane's own, behind which nothing else of the pane
    // can be: when the host has recorded it, it has recorded them all.
    pane.notify_window_size(TerminalWindowSize::new(90, 24, 9, 18));
    let recorded = watch_until("the pane's resize", &|_, resizes| {
        resizes.contains(&(90, 24))
    });
    assert_eq!(recorded, [(80, 23), (80, 24), (90, 24)]);
}

#[test]
fn a_host_that_cannot_be_reached_leaves_the_pane_to_the_window() {
    let dir = short_dir();
    let nowhere = HostEndpoint::new(dir.path().join("nobody.sock"), None);
    let params = pane_params("/bin/sh", &[]);
    let (mut pane, pending) = TerminalState::new_pending(params.cols, params.rows);
    let Opened::InWindow(Ok(spawned)) = open(Some(&nowhere), facts(), params, pending, None) else {
        unreachable!("the pane was not opened in the window");
    };
    pane.promote(spawned);
    assert!(!pane.is_in_host());
    pane.write_to_pty(b"echo LOCAL_$((40 + 2))\n".to_vec());
    wait_for_text(&mut pane, "LOCAL_42");
    // And the next pane does not wait for the same host again.
    assert!(nowhere.is_avoided());

    // No place for a host at all is the same pane.
    let params = pane_params("/bin/sh", &[]);
    let (mut pane, pending) = TerminalState::new_pending(params.cols, params.rows);
    let Opened::InWindow(Ok(spawned)) = open(None, facts(), params, pending, None) else {
        unreachable!("the pane was not opened in the window");
    };
    pane.promote(spawned);
    pane.write_to_pty(b"echo LOCAL_$((40 + 3))\n".to_vec());
    wait_for_text(&mut pane, "LOCAL_43");
}

#[test]
fn a_host_that_refuses_the_shell_leaves_the_pane_to_the_window() {
    let host = TestHost::start(|_| {});
    assert!(!host.endpoint.is_avoided());
    // A size the host refuses and the window's own spawn takes.
    let mut params = pane_params("/bin/sh", &[]);
    params.cols = 0;
    let (mut pane, pending) = TerminalState::new_pending(80, 24);
    let Opened::InWindow(Ok(spawned)) = open(Some(&host.endpoint), facts(), params, pending, None)
    else {
        unreachable!("the pane was not opened in the window");
    };
    pane.promote(spawned);
    assert!(!pane.is_in_host());
    pane.write_to_pty(b"echo LOCAL_$((40 + 2))\n".to_vec());
    wait_for_text(&mut pane, "LOCAL_42");
    assert!(host.sessions().is_empty());

    // For a while panes are opened in the window without asking, though
    // the host is there and would take this one.
    assert!(host.endpoint.is_avoided());
    let params = pane_params("/bin/sh", &[]);
    let (_pane, pending) = TerminalState::new_pending(80, 24);
    assert!(matches!(
        open(Some(&host.endpoint), facts(), params, pending, None),
        Opened::InWindow(Ok(_))
    ));
    assert!(host.sessions().is_empty());
}

#[test]
fn a_host_with_no_room_for_the_pane_starts_nothing_and_leaves_it_to_the_window() {
    // Room for the shared connection and no other.
    let host = TestHost::start(|config| config.max_connections = 1);
    // Taken first, and only once the connection that waited for the host
    // to come up is gone from the host's count.
    let control = wait_for("the shared connection", || {
        host.endpoint.for_existing_session()
    });
    let params = pane_params("/bin/sh", &[]);
    let (mut pane, pending) = TerminalState::new_pending(80, 24);
    let Opened::InWindow(Ok(spawned)) = open(Some(&host.endpoint), facts(), params, pending, None)
    else {
        unreachable!("the pane was not opened in the window");
    };
    pane.promote(spawned);
    pane.write_to_pty(b"echo LOCAL_$((40 + 2))\n".to_vec());
    wait_for_text(&mut pane, "LOCAL_42");
    assert!(host.endpoint.is_avoided());
    assert!(control.list().expect("a list").is_empty());
}

/// After the host has started the shell there is no going back to a PTY in
/// the window: that would be a second shell. The first one is ended, and
/// the pane has an error to show.
#[test]
fn a_shell_the_host_started_and_the_pane_lost_is_ended_and_not_started_again() {
    let host = TestHost::start(|_| {});
    // A connection for the pane that cannot follow the session: it leads
    // to another host, which has never heard of it.
    let other = TestHost::start(|_| {});
    let control = host.client();
    let params = sh("echo started; sleep 300");
    let (mut pane, pending) = TerminalState::new_pending(80, 24);
    let lost = start_and_follow(
        &params,
        &pending,
        &host.endpoint,
        &control,
        other.client(),
        facts(),
    );
    let Err(OpenError::AfterSpawn(error)) = lost else {
        unreachable!("the pane was attached to a session of another host");
    };
    assert!(
        format!("{error:#}").contains("cannot attach to the session"),
        "{error:#}"
    );
    // No shell in the window either.
    pane.sync();
    assert!(!pane.notifier.0.is_live());
    assert_eq!(pane.child_pid, 0);
    wait_for("the host to forget the session", || {
        host.sessions().is_empty().then_some(())
    });
    assert!(other.sessions().is_empty());
}

#[test]
fn the_facts_a_window_states_are_the_colors_a_pane_answers_with() {
    let theme = crate::theme::active_theme();
    let rgb = |color: gpui::Hsla| {
        let rgb = hsla_to_alac_rgb(color);
        Rgb(rgb.r, rgb.g, rgb.b)
    };
    let stated = window_facts(&theme, None, None);
    assert_eq!(stated.foreground, Some(rgb(theme.foreground)));
    assert_eq!(stated.background, Some(rgb(theme.ansi_background)));
    assert_eq!(stated.cursor, Some(rgb(theme.cursor)));
    assert_eq!(stated.palette.len(), 256);
    assert_eq!(
        stated.palette.get(&1),
        Some(&rgb(palette_color_at(1, &theme)))
    );
    assert_eq!(
        stated.palette.get(&255),
        Some(&rgb(palette_color_at(255, &theme)))
    );
    // Not known before the first layout, and then not stated.
    assert_eq!((stated.cell_width, stated.cell_height), (None, None));

    let cursor = gpui::Hsla::from(gpui::rgb(0x00ff7f));
    let stated = window_facts(&theme, Some(cursor), Some((9, 18)));
    assert_eq!(stated.cursor, Some(Rgb(0x00, 0xff, 0x7f)));
    assert_eq!((stated.cell_width, stated.cell_height), (Some(9), Some(18)));
}
