//! Tests for a terminal whose PTY is in the session host.
//!
//! A child module of `pty_session`, so that it can look at a terminal's
//! grid and at the events its emulator emits. Nothing here asserts by
//! sleeping: every wait is a poll with a deadline.

use splitlane_host::{ExitStatus, StreamEvent};

use super::*;

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
/// bell, a dev server's address, and questions to the terminal.
fn a_busy_session(word: &str, port: u16) -> Vec<u8> {
    format!(
        "\x1b]0;{word}-title\x07\
         \x1b]7;file://localhost/tmp/{word}\x07\
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
