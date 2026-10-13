//! Tests of a Ghostty runtime whose PTY is in the session host.
//!
//! No host and no child: a test is the stream. It hands the sink the events
//! a host would send, holds the receiving end of the pane's queue to the
//! host, and looks at what the runtime published and at what it said.
//! Nothing here asserts by sleeping. A request the runtime answers is
//! queued behind everything queued before it, so its answer is the moment
//! all of that has been done.

use futures::channel::mpsc::UnboundedReceiver;

use super::super::super::host_session::{RemoteReceiver, remote_queue};
use super::*;

const DEADLINE: Duration = Duration::from_secs(20);

/// A pane of a session in the host, with the test at both ends of it.
struct Pane {
    session: GhosttySession,
    sink: GhosttySink,
    events: UnboundedReceiver<GhosttyUiEvent>,
    /// What the pane said to the host.
    said: RemoteReceiver,
}

/// Where a stream of these tests begins: at the start of a log that held
/// `tail_seq` positions when the attach was made, on a grid of 40 by 10.
fn start_at(tail_seq: u64) -> StreamStart {
    StreamStart {
        start_seq: 0,
        tail_seq: Some(tail_seq),
        gap: false,
        cols: 40,
        rows: 10,
    }
}

fn output(seq: u64, bytes: &[u8]) -> StreamEvent {
    StreamEvent::Output {
        seq,
        bytes: bytes.to_vec(),
    }
}

impl Pane {
    fn open(mode: AttachMode, start: StreamStart) -> Self {
        Self::open_with_room_for(mode, start, 1024 * 1024)
    }

    /// A pane whose queue to the host takes `input_limit` bytes of input.
    fn open_with_room_for(mode: AttachMode, start: StreamStart, input_limit: usize) -> Self {
        // Focused and allowed to copy, so that what is not copied is held
        // back by the replay and by nothing else.
        let gate = Arc::new(ClipboardGate::default());
        gate.set_policy(true, false);
        gate.set_focused(true);
        let (session, pending, events) = GhosttySession::pending_with_clipboard_gate(
            TerminalWindowSize::new(80, 24, 0, 0),
            gate,
        );
        let target = GhosttyAttach {
            session: session.clone(),
            pending,
            max_scrollback: 1000,
        };
        let (sink, ()) = target.sink(mode, start);
        let (sender, said) = remote_queue(input_limit);
        session.attach_host(sender);
        session.promote();
        let mut pane = Self {
            session,
            sink,
            events,
            said,
        };
        // A runtime says once that its empty grid can be drawn, before it
        // reads anything. Taken here, so that every test starts with what
        // its own stream caused.
        match pane.events.try_recv() {
            Ok(GhosttyUiEvent::Wakeup(state)) => state.acknowledge_wakeup(),
            other => unreachable!("a runtime starts with a wakeup, not {other:?}"),
        }
        pane
    }

    fn feed(&mut self, seq: u64, bytes: &[u8]) -> SinkStep {
        self.sink.handle(output(seq, bytes))
    }

    /// Wait until the runtime has done everything queued so far.
    fn settle(&self) {
        let _ = self.session.extract_text();
    }

    /// The grid as the window would draw it, row by row.
    fn rows(&self) -> Vec<String> {
        self.settle();
        let state = self.session.inner.state.read();
        let content = &state.content;
        let mut rows = vec![vec![' '; content.cols]; content.rows];
        for cell in content.cells.iter() {
            let line = usize::try_from(cell.point.line.0).ok();
            if let Some(slot) = line
                .and_then(|line| rows.get_mut(line))
                .and_then(|row| row.get_mut(cell.point.column.0))
            {
                *slot = cell.c;
            }
        }
        rows.into_iter()
            .map(|row| row.into_iter().collect::<String>().trim_end().to_owned())
            .collect()
    }

    fn screen(&self) -> String {
        self.rows().join("\n")
    }

    fn grid_size(&self) -> (usize, usize) {
        self.settle();
        let state = self.session.inner.state.read();
        (state.content.cols, state.content.rows)
    }

    /// Everything the runtime has told the window since this was last
    /// asked, by name. Wakeups are acknowledged, as the window does, so
    /// that the next one is sent.
    fn told_window(&mut self) -> Vec<String> {
        self.settle();
        self.told_so_far()
    }

    /// [`Self::told_window`] without waiting for the runtime: what has
    /// arrived by now.
    fn told_so_far(&mut self) -> Vec<String> {
        let mut told = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            told.push(match event {
                GhosttyUiEvent::Wakeup(state) => {
                    state.acknowledge_wakeup();
                    "wakeup".to_owned()
                }
                GhosttyUiEvent::Title(state) => {
                    format!("title:{}", state.take_title().unwrap_or_default())
                }
                GhosttyUiEvent::WorkingDirectory(state) => {
                    format!("cwd:{}", state.take_working_directory().unwrap_or_default())
                }
                GhosttyUiEvent::Clipboard(state) => {
                    format!("copied:{}", state.take_clipboard().join("+"))
                }
                GhosttyUiEvent::ServiceOutputReady(state) => {
                    state.acknowledge_service_output();
                    "service-output".to_owned()
                }
                GhosttyUiEvent::ChildExited { code, signal } => {
                    format!("exited:{code}:{}", signal.unwrap_or_default())
                }
                GhosttyUiEvent::ReplayFinished => "replay-finished".to_owned(),
                GhosttyUiEvent::InputRejected(error) => format!("input-rejected:{error}"),
                GhosttyUiEvent::RuntimeFailed(error) => format!("failed:{error}"),
            });
        }
        told
    }

    /// Wait for the runtime to say its child ended, and return everything
    /// it told the window up to then.
    fn told_window_until_the_end(&mut self) -> Vec<String> {
        let deadline = Instant::now() + DEADLINE;
        let mut told = Vec::new();
        loop {
            // Not by asking the runtime: it stops when it has said this,
            // and a question queued behind its last message is never
            // answered.
            told.extend(self.told_so_far());
            if told.iter().any(|event| event.starts_with("exited:")) {
                // Its queue closes as it stops, which is the moment
                // nothing more can be asked of it.
                while self.sink.mailbox.send_host(HostEvent::GoLive) {
                    assert!(Instant::now() < deadline, "the runtime never stopped");
                    std::thread::sleep(Duration::from_millis(1));
                }
                return told;
            }
            assert!(
                Instant::now() < deadline,
                "the pane never ended; it said {told:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// What the pane has said to the host since this was last asked.
    fn said_to_host(&self) -> Vec<Said> {
        self.settle();
        let mut said = Vec::new();
        while let Some(message) = self.said.try_recv() {
            said.push(match message {
                RemoteMsg::Input(bytes) => Said::Input(bytes.into_owned()),
                RemoteMsg::Resize {
                    cols,
                    rows,
                    pixel_width,
                    pixel_height,
                } => Said::Resize(cols, rows, pixel_width, pixel_height),
                RemoteMsg::Facts(_) => Said::Other("facts"),
                RemoteMsg::Redraw => Said::Other("redraw"),
                RemoteMsg::Shutdown => Said::Other("shutdown"),
            });
        }
        said
    }

    fn mark_count(&self) -> usize {
        self.settle();
        self.session
            .marks()
            .lock()
            .expect("the marks")
            .iter()
            .count()
    }

    /// Where the resize the window asked for stands: what it asked for,
    /// whether a request is still under way, and what was applied.
    fn resize_state(&self) -> (TerminalWindowSize, bool, Option<TerminalWindowSize>) {
        self.settle();
        let resize = self.session.inner.resize.lock().expect("the resize state");
        (resize.requested, resize.submitted.is_some(), resize.applied)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Said {
    Input(Vec<u8>),
    /// Columns, rows, and the text area in pixels.
    Resize(u16, u16, u16, u16),
    Other(&'static str),
}

fn input(bytes: &[u8]) -> Said {
    Said::Input(bytes.to_vec())
}

fn key(character: char) -> ghostty::KeyInput {
    ghostty::KeyInput {
        key: ghostty::Key::Character(character),
        action: ghostty::KeyAction::Press,
        modifiers: ghostty::Modifiers::empty(),
        consumed_modifiers: ghostty::Modifiers::empty(),
        text: character.to_string(),
        unshifted_codepoint: Some(character),
        composing: false,
    }
}

/// Everything a session can have printed that a window acts on: a title,
/// a working directory, a command with its marks, a copy to the clipboard,
/// and questions to the terminal.
fn a_busy_session(word: &str) -> Vec<u8> {
    format!(
        "\x1b]0;{word}-title\x07\
         \x1b]7;file://localhost/tmp/{word}\x07\
         \x1b]133;A\x07$ \x1b]133;B\x07serve\r\n\
         \x1b]133;E;serve {word}\x07\x1b]133;C\x07\
         \x1b]52;c;{copied}\x07\
         \x1b[6n\x1b]11;?\x07\x1b[5n\x1b]7501;?\x1b\\\x1b[c\
         {word}: Local: http://localhost:5173/\r\n\
         \x1b]133;D;0\x07",
        copied = match word {
            "old" => "b2xk",
            _ => "bmV3",
        },
    )
    .into_bytes()
}

// ---------------------------------------------------------------------------
// The queue: one order, and room for everything that is put on it.
// ---------------------------------------------------------------------------

#[test]
fn what_a_stream_says_is_queued_in_the_order_it_was_said() {
    let mailbox = RuntimeMailbox::new();
    assert!(mailbox.send_stream_output(b"before"));
    assert!(mailbox.send_host(HostEvent::Resize { cols: 20, rows: 5 }));
    assert!(mailbox.send_stream_output(b"between"));
    // A command of the window's, queued by another thread in the middle.
    assert!(
        mailbox
            .try_send_control(RuntimeMessage::ClearSelection)
            .is_ok()
    );
    assert!(mailbox.send_host(HostEvent::GoLive));
    assert!(mailbox.send_stream_output(b"after"));

    // A run of output ends at the next thing that is not output, so that
    // a batch never reads past an event.
    assert!(matches!(
        mailbox.try_recv(),
        Ok(RuntimeMessage::Output(bytes)) if bytes == b"before"
    ));
    assert!(mailbox.try_recv_consecutive_output().is_none());
    let rest: Vec<String> = mailbox
        .drain()
        .into_iter()
        .map(|message| match message {
            RuntimeMessage::Output(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            RuntimeMessage::Host(event) => format!("{event:?}"),
            RuntimeMessage::ClearSelection => "clear-selection".to_owned(),
            _ => "other".to_owned(),
        })
        .collect();
    assert_eq!(
        rest,
        [
            "Resize { cols: 20, rows: 5 }",
            "between",
            "clear-selection",
            "GoLive",
            "after"
        ]
    );
}

#[test]
fn output_larger_than_a_buffer_is_queued_whole_and_in_order() {
    let mailbox = Arc::new(RuntimeMailbox::new());
    let sent: Vec<u8> = (0..OUTPUT_CHUNK_BYTES * 2 + 17)
        .map(|index| (index % 251) as u8)
        .collect();
    let feeder = std::thread::spawn({
        let (mailbox, sent) = (mailbox.clone(), sent.clone());
        move || mailbox.send_stream_output(&sent)
    });
    let mut received = Vec::new();
    while received.len() < sent.len() {
        match mailbox.recv_timeout(DEADLINE) {
            Ok(RuntimeMessage::Output(bytes)) => {
                assert!(bytes.len() <= OUTPUT_CHUNK_BYTES);
                received.extend_from_slice(&bytes);
                mailbox.recycle_output_buffer(bytes);
            }
            other => unreachable!("only output was sent: {}", other.is_ok()),
        }
    }
    assert!(feeder.join().expect("the feeder"));
    assert_eq!(received, sent);
}

/// The reader takes nothing more from the stream while the runtime has not
/// caught up: that is what leaves the rest on the host's socket.
#[test]
fn a_reader_waits_for_room_and_is_let_go_when_the_runtime_stops() {
    // Events of the stream.
    let mailbox = Arc::new(RuntimeMailbox::new());
    for _ in 0..HOST_EVENT_CAPACITY {
        assert!(mailbox.send_host(HostEvent::Resize { cols: 80, rows: 24 }));
    }
    let (sent_tx, sent_rx) = sync_channel(1);
    let reader = std::thread::spawn({
        let mailbox = mailbox.clone();
        move || {
            let _ = sent_tx.send(mailbox.send_host(HostEvent::GoLive));
        }
    });
    assert!(matches!(
        sent_rx.recv_timeout(Duration::from_millis(20)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(matches!(
        mailbox.recv_timeout(Duration::ZERO),
        Ok(RuntimeMessage::Host(HostEvent::Resize { .. }))
    ));
    assert_eq!(sent_rx.recv_timeout(DEADLINE), Ok(true));
    reader.join().expect("the reader");

    // Output, with every buffer queued.
    let mailbox = Arc::new(RuntimeMailbox::new());
    for _ in 0..OUTPUT_BUFFER_COUNT {
        assert!(mailbox.send_stream_output(b"x"));
    }
    let (sent_tx, sent_rx) = sync_channel(1);
    let reader = std::thread::spawn({
        let mailbox = mailbox.clone();
        move || {
            let _ = sent_tx.send(mailbox.send_stream_output(b"one more"));
        }
    });
    assert!(matches!(
        sent_rx.recv_timeout(Duration::from_millis(20)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    // The runtime going away is what ends the wait, and the reader is
    // told that nothing was taken.
    mailbox.close();
    assert_eq!(sent_rx.recv_timeout(DEADLINE), Ok(false));
    reader.join().expect("the reader");
    assert!(!mailbox.send_host(HostEvent::Lost));
}

// ---------------------------------------------------------------------------
// The runtime, driven by a scripted stream.
// ---------------------------------------------------------------------------

#[test]
fn the_panes_terminal_never_answers_the_program() {
    let mut pane = Pane::open(AttachMode::Fresh, start_at(0));
    // Where is the cursor, what is the background, are you there, do you
    // read status reports, what are you: a pane that owns its PTY writes
    // an answer to each of them.
    pane.feed(
        0,
        b"asked\x1b[6n\x1b]11;?\x07\x1b[5n\x1b]7501;?\x1b\\\x1b[c\x1b[>c\x1b[18t and waiting",
    );
    assert_eq!(pane.rows()[0], "asked and waiting");
    assert_eq!(pane.said_to_host(), []);

    // The same holds for a terminal that is told to answer by the modes
    // the program set, and after the window sent input of its own.
    pane.feed(60, b"\x1b[?1004h\x1b[?2004h\x1b[6n");
    assert!(pane.session.write(b"typed".to_vec()).is_sent());
    pane.feed(80, b"\x1b[6n\x1b]10;?\x1b\\");
    assert_eq!(pane.said_to_host(), [input(b"typed")]);
    assert_eq!(pane.session.refused_input(), 0);
}

#[test]
fn input_reaches_the_host_encoded_by_the_terminal_and_in_the_order_it_was_written() {
    let mut pane = Pane::open(AttachMode::Fresh, start_at(0));
    assert!(pane.session.write(b"raw ".to_vec()).is_sent());
    assert!(pane.session.write_key(key('k')).is_sent());
    assert!(
        pane.session
            .write_paste("pasted".to_owned(), true)
            .is_sent()
    );
    assert!(pane.session.write(b" end".to_vec()).is_sent());
    assert_eq!(
        pane.said_to_host(),
        [
            input(b"raw "),
            input(b"k"),
            input(b"pasted"),
            input(b" end")
        ]
    );

    // The program asks for pastes to be bracketed, and the next one is:
    // the encoding is the terminal's, for the modes in effect now.
    pane.feed(0, b"\x1b[?2004h");
    assert!(pane.session.write_paste("again".to_owned(), true).is_sent());
    assert_eq!(pane.said_to_host(), [input(b"\x1b[200~again\x1b[201~")]);
    // What was handed over no longer counts as waiting here.
    assert_eq!(pane.session.queued_input_bytes(), 0);
}

#[test]
fn input_the_host_has_no_room_for_is_dropped_and_the_pane_goes_on() {
    let mut pane = Pane::open_with_room_for(AttachMode::Fresh, start_at(0), 8);
    assert!(pane.session.write(b"12345".to_vec()).is_sent());
    // Would make eleven: refused by the queue to the host, after this
    // runtime had taken it.
    assert!(pane.session.write(b"678901".to_vec()).is_sent());
    assert!(pane.session.write(b"abc".to_vec()).is_sent());
    pane.settle();
    assert_eq!(pane.session.refused_input(), 1);
    // Accepted or not, nothing is still counted as waiting in the runtime.
    assert_eq!(pane.session.queued_input_bytes(), 0);
    assert_eq!(pane.said_to_host(), [input(b"12345"), input(b"abc")]);

    // A refusal is not a failure: the pane still reads its stream, takes
    // input and has not ended.
    pane.feed(0, b"still here");
    assert_eq!(pane.rows()[0], "still here");
    assert!(pane.session.write(b"more".to_vec()).is_sent());
    assert_eq!(pane.said_to_host(), [input(b"more")]);
    let told = pane.told_window();
    assert!(
        !told
            .iter()
            .any(|event| event.starts_with("exited:") || event.starts_with("failed:")),
        "{told:?}"
    );
}

#[test]
fn a_resize_sets_the_grid_and_tells_the_host_once() {
    let mut pane = Pane::open(AttachMode::Fresh, start_at(0));
    // Nothing is said before the window has a size of its own to say.
    assert_eq!(pane.said_to_host(), []);

    let size = TerminalWindowSize::new(100, 30, 9, 18);
    pane.session.resize(size);
    assert_eq!(pane.grid_size(), (100, 30));
    assert_eq!(pane.said_to_host(), [Said::Resize(100, 30, 900, 540)]);
    // The request is done with: nothing under way, and what was asked for
    // is what was applied.
    assert_eq!(pane.resize_state(), (size, false, Some(size)));
    assert!(pane.told_window().contains(&"wakeup".to_owned()));

    // The same size again is not a request, and not news for the host.
    pane.session.resize(size);
    pane.session.retry_backpressured_commands();
    assert_eq!(pane.said_to_host(), []);

    // Cells of another size on the same grid are: the program is told
    // the text area changed.
    let zoomed = TerminalWindowSize::new(100, 30, 11, 22);
    pane.session.resize(zoomed);
    assert_eq!(pane.said_to_host(), [Said::Resize(100, 30, 1100, 660)]);
    assert_eq!(pane.resize_state(), (zoomed, false, Some(zoomed)));

    // A size the host did not confirm is sent again when asked.
    pane.session.resend_host_size();
    assert_eq!(pane.said_to_host(), [Said::Resize(100, 30, 1100, 660)]);
}

#[test]
fn a_size_asked_for_before_the_pane_has_its_queue_is_sent_when_it_gets_one() {
    let gate = Arc::new(ClipboardGate::default());
    let (session, pending, _events) =
        GhosttySession::pending_with_clipboard_gate(TerminalWindowSize::new(80, 24, 0, 0), gate);
    let target = GhosttyAttach {
        session: session.clone(),
        pending,
        max_scrollback: 1000,
    };
    let (_sink, ()) = target.sink(AttachMode::Fresh, start_at(0));
    // The window lays the pane out while its shell is still being started.
    let size = TerminalWindowSize::new(90, 20, 8, 16);
    session.resize(size);
    let _ = session.extract_text();
    assert_eq!(session.inner.state.read().content.cols, 90);

    let (sender, said) = remote_queue(1024);
    session.attach_host(sender);
    let _ = session.extract_text();
    assert!(matches!(
        said.try_recv(),
        Some(RemoteMsg::Resize {
            cols: 90,
            rows: 20,
            pixel_width: 720,
            pixel_height: 320,
        })
    ));
    assert!(said.try_recv().is_none());
}

#[test]
fn the_first_layout_clears_a_new_pane_and_never_one_attached_to_a_session() {
    let size = TerminalWindowSize::new(100, 30, 9, 18);

    // A pane the window started: what its shell printed into a grid of the
    // wrong size is cleared when the window gives the grid its own.
    let mut fresh = Pane::open(AttachMode::Fresh, start_at(0));
    fresh.feed(0, b"printed too early\r\n");
    assert_eq!(fresh.rows()[0], "printed too early");
    let (_, consumed) = fresh.session.render_content(size, 0, 0, true);
    assert!(consumed);
    assert_eq!(fresh.screen().trim(), "");
    assert_eq!(fresh.grid_size(), (100, 30));
    assert_eq!(fresh.resize_state(), (size, false, Some(size)));

    // A pane attached to a session shows what the session printed. A view
    // that asked for the clear all the same does not get it.
    let old = b"what the session printed\r\n";
    let mut attached = Pane::open(AttachMode::Reattach, start_at(old.len() as u64));
    attached.feed(0, old);
    assert_eq!(attached.rows()[0], "what the session printed");
    let (_, consumed) = attached.session.render_content(size, 0, 0, true);
    assert!(consumed);
    assert_eq!(attached.rows()[0], "what the session printed");
    assert_eq!(attached.grid_size(), (100, 30));
    // The request was completed all the same, once: nothing is left to
    // be asked again.
    assert_eq!(attached.resize_state(), (size, false, Some(size)));
    let resize = attached
        .session
        .inner
        .resize
        .lock()
        .expect("the resize state");
    assert!(!resize.clear_initial_requested);
}

#[test]
fn replaying_a_log_rebuilds_the_grid_and_acts_on_nothing_until_it_ends() {
    let old = a_busy_session("old");
    let new = a_busy_session("new");
    // The log held all of `old` and nothing of `new` when the attach was
    // made.
    let mut pane = Pane::open(AttachMode::Reattach, start_at(old.len() as u64));
    assert_eq!(pane.told_window(), Vec::<String>::new());

    // All but the last byte of the log: old output, every byte of it.
    let (most, last) = old.split_at(old.len() - 1);
    pane.feed(0, most);
    // Nothing was reported, nobody was woken, nothing was copied and no
    // mark was placed; and the program got no answer from here.
    assert_eq!(pane.told_window(), Vec::<String>::new());
    assert_eq!(pane.mark_count(), 0);
    assert_eq!(pane.said_to_host(), []);
    // What the window draws is still what it drew before the replay.
    assert_eq!(pane.screen().trim(), "");

    // The last old byte ends the replay. Of what the log reported, the
    // last of each kind is reported now, once, and the window is told
    // there is something to draw - which is not output, so no wakeup.
    pane.feed(most.len() as u64, last);
    assert_eq!(
        pane.told_window(),
        ["title:old-title", "cwd:/tmp/old", "replay-finished"]
    );
    assert!(pane.screen().contains("old: Local:"), "{}", pane.screen());
    // One mark for everything the log ran, where the cursor is now.
    assert_eq!(pane.mark_count(), 1);

    // What is new is acted on as it happens.
    pane.feed(old.len() as u64, &new);
    let told = pane.told_window();
    for expected in ["wakeup", "title:new-title", "cwd:/tmp/new", "copied:new"] {
        assert!(told.contains(&expected.to_owned()), "{expected}: {told:?}");
    }
    assert!(!told.contains(&"replay-finished".to_owned()), "{told:?}");
    let screen = pane.screen();
    let old_at = screen.find("old: Local:").expect("the old output");
    let new_at = screen.find("new: Local:").expect("the new output");
    assert!(old_at < new_at, "{screen}");
    assert!(pane.mark_count() > 1, "{}", pane.mark_count());
    // The newest command is the newest command, whenever it ran.
    assert_eq!(
        pane.session
            .last_command()
            .lock()
            .expect("the last command")
            .as_deref(),
        Some("serve new")
    );
    // Old or new, the pane answered none of the questions.
    assert_eq!(pane.said_to_host(), []);
}

/// One chunk can hold the last old bytes and the first new ones.
#[test]
fn a_chunk_across_the_end_of_the_log_is_old_up_to_it_and_new_after() {
    let old = a_busy_session("old");
    let new = a_busy_session("new");
    let mut pane = Pane::open(AttachMode::Reattach, start_at(old.len() as u64));
    let mut both = old.clone();
    both.extend_from_slice(&new);
    pane.feed(0, &both);

    let told = pane.told_window();
    // Copied once, and it is the new one.
    assert_eq!(
        told.iter()
            .filter(|event| event.starts_with("copied:"))
            .collect::<Vec<_>>(),
        ["copied:new"]
    );
    assert_eq!(
        told.iter()
            .filter(|event| *event == "replay-finished")
            .count(),
        1
    );
    let position = |name: &str| told.iter().position(|event| event == name);
    assert!(
        position("replay-finished") < position("copied:new"),
        "{told:?}"
    );
    assert!(position("replay-finished") < position("wakeup"), "{told:?}");
    let screen = pane.screen();
    assert!(screen.contains("old: Local:"), "{screen}");
    assert!(screen.contains("new: Local:"), "{screen}");
    assert_eq!(pane.said_to_host(), []);
}

#[test]
fn a_replay_keeps_one_mark_for_everything_the_log_ran() {
    let old = a_busy_session("old");
    let mut pane = Pane::open(AttachMode::Reattach, start_at(old.len() as u64));
    pane.feed(0, &old);
    assert_eq!(
        pane.told_window(),
        ["title:old-title", "cwd:/tmp/old", "replay-finished"]
    );
    assert_eq!(pane.mark_count(), 1);
}

#[test]
fn a_log_is_replayed_at_the_sizes_it_recorded_and_ends_at_the_windows() {
    // The stream starts on a grid of 40 by 10; the pane was made at 80 by
    // 24. A character sent to the far corner lands in the last column of
    // whatever grid there is when it is read.
    let corner = |mark: char| format!("\r\n\x1b[999;999H{mark}\r").into_bytes();
    let (first, second, third) = (corner('a'), corner('b'), corner('c'));
    let tail = (first.len() + 1 + second.len()) as u64;
    let mut pane = Pane::open(AttachMode::Reattach, start_at(tail));

    // The window lays the pane out in the middle of the replay.
    let size = TerminalWindowSize::new(100, 30, 9, 18);
    pane.feed(0, &first);
    pane.session.resize(size);
    // The request is done with at once, and nothing is said to the host
    // yet: the log has the grid until it ends.
    assert_eq!(pane.resize_state(), (size, false, Some(size)));
    assert_eq!(pane.said_to_host(), []);

    // A resize the log recorded, between the bytes it fell between.
    let resize_at = first.len() as u64;
    pane.sink.handle(StreamEvent::Resized {
        seq: resize_at,
        cols: 60,
        rows: 12,
    });
    pane.feed(resize_at + 1, &second);
    assert_eq!(pane.told_window(), ["replay-finished"]);

    // Live: the grid is the window's, and the host is told so, once.
    assert_eq!(pane.grid_size(), (100, 30));
    assert_eq!(pane.said_to_host(), [Said::Resize(100, 30, 900, 540)]);
    pane.feed(tail, &third);
    // The echo of that request, and a size somebody else gave the
    // session, are not the window's to follow.
    pane.sink.handle(StreamEvent::Resized {
        seq: tail + third.len() as u64,
        cols: 33,
        rows: 7,
    });
    assert_eq!(pane.grid_size(), (100, 30));
    assert_eq!(pane.said_to_host(), []);

    let column_of = |mark: char| {
        pane.rows()
            .iter()
            .find_map(|row| row.chars().position(|c| c == mark))
            .unwrap_or_else(|| unreachable!("{mark} is not on the grid:\n{}", pane.screen()))
    };
    assert_eq!(column_of('a'), 39);
    assert_eq!(column_of('b'), 59);
    assert_eq!(column_of('c'), 99);
}

#[test]
fn a_session_with_nothing_to_replay_is_live_at_once() {
    // An idle session: no event may ever come to end the replay.
    let mut pane = Pane::open(AttachMode::Reattach, start_at(0));
    assert_eq!(pane.told_window(), ["replay-finished"]);
    pane.feed(0, b"\x1b]0;now\x07now");
    let told = pane.told_window();
    assert!(told.contains(&"title:now".to_owned()), "{told:?}");
    assert!(told.contains(&"wakeup".to_owned()), "{told:?}");
    assert_eq!(pane.rows()[0], "now");
}

#[test]
fn a_log_that_lost_its_start_asks_for_one_repaint_when_it_ends() {
    let start = StreamStart {
        start_seq: 500,
        tail_seq: Some(504),
        gap: true,
        cols: 40,
        rows: 10,
    };
    let mut pane = Pane::open(AttachMode::Reattach, start);
    // The modes in effect where the stream starts, behind a cancel that
    // ends whatever sequence the cut left open.
    assert_eq!(
        pane.sink
            .handle(StreamEvent::Preamble(b"\x18\x1b[?2004h".to_vec())),
        SinkStep::Continue
    );
    assert_eq!(pane.told_window(), Vec::<String>::new());
    assert_eq!(pane.feed(500, b"tail"), SinkStep::Redraw);
    assert_eq!(pane.told_window(), ["replay-finished"]);
    assert_eq!(pane.rows()[0], "tail");
    // The mode the preamble set is the terminal's now.
    assert!(pane.session.write_paste("p".to_owned(), true).is_sent());
    assert_eq!(pane.said_to_host(), [input(b"\x1b[200~p\x1b[201~")]);
}

#[test]
fn the_end_of_the_child_is_published_after_its_last_output() {
    let exit = |code, signal| ExitStatus { code, signal };

    let mut pane = Pane::open(AttachMode::Fresh, start_at(0));
    pane.feed(0, b"last words\r\n");
    assert_eq!(
        pane.sink.handle(StreamEvent::Exited(exit(Some(3), None))),
        SinkStep::Ended
    );
    let told = pane.told_window_until_the_end();
    assert_eq!(told.last().map(String::as_str), Some("exited:3:"));
    // On the grid by the time the exit is known.
    assert_eq!(pane.rows()[0], "last words", "{told:?}");
    assert_eq!(
        told.iter()
            .filter(|event| event.starts_with("exited:"))
            .count(),
        1
    );

    // A child that was killed, as a pane that owns its PTY reports one.
    let mut killed = Pane::open(AttachMode::Fresh, start_at(0));
    killed
        .sink
        .handle(StreamEvent::Exited(exit(None, Some(libc::SIGTERM))));
    assert_eq!(
        killed
            .told_window_until_the_end()
            .last()
            .map(String::as_str),
        Some("exited:1:Terminated")
    );

    // A host that cannot say how the child ended still says that it did.
    let mut unknown = Pane::open(AttachMode::Fresh, start_at(0));
    unknown.sink.handle(StreamEvent::Exited(exit(None, None)));
    assert_eq!(
        unknown
            .told_window_until_the_end()
            .last()
            .map(String::as_str),
        Some("exited:-1:")
    );

    // A session that had ended before the window attached: its log, and
    // then its end, with nothing acted on in between.
    let old = a_busy_session("old");
    let mut ended = Pane::open(AttachMode::Reattach, start_at(old.len() as u64 + 50));
    ended.feed(0, &old);
    ended.sink.handle(StreamEvent::Exited(exit(Some(0), None)));
    let told = ended.told_window_until_the_end();
    assert_eq!(
        told,
        [
            "title:old-title",
            "cwd:/tmp/old",
            "replay-finished",
            "wakeup",
            "exited:0:"
        ]
    );
}

#[test]
fn a_lost_connection_ends_the_pane_and_says_why() {
    let mut pane = Pane::open(AttachMode::Fresh, start_at(0));
    pane.feed(0, b"working");
    pane.sink.connection_lost();
    let told = pane.told_window_until_the_end();
    assert_eq!(told.last().map(String::as_str), Some("exited:-1:"));
    let text = pane.screen();
    assert!(text.contains("working"), "{text}");
    assert!(
        text.contains("The connection to the session host was lost."),
        "{text}"
    );
    // Nothing was said to the host on the way out, and nothing can be
    // said to a pane that ended.
    assert_eq!(pane.said_to_host(), []);

    // In the middle of a replay the pane still shows what it had read.
    let mut replaying = Pane::open(AttachMode::Reattach, start_at(1000));
    replaying.feed(0, b"half a log");
    replaying.sink.connection_lost();
    let told = replaying.told_window_until_the_end();
    assert!(told.contains(&"replay-finished".to_owned()), "{told:?}");
    // On the grid the log left, which is narrower than the notice.
    let text = replaying.screen().replace('\n', "");
    assert!(text.contains("half a log"), "{text}");
    assert!(text.contains("was lost."), "{text}");
}

/// A pane that is closed stops its runtime, and that is all it does: what
/// ends the session is the pane's handle, which the runtime does not have.
#[test]
fn shutting_the_runtime_down_says_nothing_to_the_host_and_frees_the_reader() {
    let mut pane = Pane::open(AttachMode::Fresh, start_at(0));
    pane.feed(0, b"running");
    pane.settle();
    pane.session.shutdown();
    // The queue closes when the runtime has stopped; from then on the
    // reader is told at once that nothing takes the stream.
    let deadline = Instant::now() + DEADLINE;
    while pane
        .sink
        .mailbox
        .send_host(HostEvent::Resize { cols: 1, rows: 1 })
    {
        assert!(Instant::now() < deadline, "the runtime never stopped");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!pane.sink.mailbox.send_stream_output(b"late"));
    assert_eq!(pane.feed(7, b"late"), SinkStep::Continue);
    let mut said = Vec::new();
    while let Some(message) = pane.said.try_recv() {
        said.push(matches!(message, RemoteMsg::Shutdown));
    }
    assert_eq!(said, Vec::<bool>::new());
    // No exit was made up for a child nobody saw end.
    let mut ended = false;
    while let Ok(event) = pane.events.try_recv() {
        ended |= matches!(event, GhosttyUiEvent::ChildExited { .. });
    }
    assert!(!ended);
}

#[test]
fn the_shell_a_host_is_asked_for_is_the_one_a_pane_starts_itself() {
    let mut params = crate::terminal::TerminalState::resolve_spawn_params(
        Some(std::path::PathBuf::from("/")),
        7,
        11,
        Some((80, 24)),
        None,
    );
    params.shell = "/bin/sh".to_owned();
    params
        .env
        .insert("PANE_ONLY".to_owned(), "from the pane".to_owned());

    // What the PTY library is handed for a pane that owns its PTY.
    let command = shell_command(&params);
    let mut local: Vec<(String, String)> = command
        .iter_full_env_as_str()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    local.sort();
    let ShellStart { mut env, cwd } =
        <GhosttyAttach as AttachTarget>::shell_start(&params).expect("a shell to ask for");
    env.sort();
    assert_eq!(env, local);
    assert_eq!(cwd, std::path::PathBuf::from("/"));

    let value = |key: &str| {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(value("TERM_PROGRAM"), Some("ghostty"));
    assert_eq!(
        value("TERM_PROGRAM_VERSION"),
        Some(ghostty::GHOSTTY_APP_VERSION)
    );
    assert_eq!(value("TERM"), Some("xterm-256color"));
    assert_eq!(value("COLORTERM"), Some("truecolor"));
    assert_eq!(value("SPLITLANE_SURFACE_ID"), Some("11"));
    assert_eq!(value("SPLITLANE_WORKSPACE_ID"), Some("7"));
    assert_eq!(value("PANE_ONLY"), Some("from the pane"));
    // The PTY library gives a shell one even in a window started with
    // none.
    assert!(value("SHELL").is_some_and(|shell| shell.starts_with('/')));
    // Nothing of the other terminal's spawn.
    assert_eq!(
        value("ALACRITTY_WINDOW_ID"),
        std::env::var("ALACRITTY_WINDOW_ID").ok().as_deref()
    );

    // A directory that is not there: the PTY library starts the shell in
    // the user's home, and the host is asked for the same.
    params.cwd = std::path::PathBuf::from("/no/such/directory/anywhere");
    let ShellStart { cwd, .. } =
        <GhosttyAttach as AttachTarget>::shell_start(&params).expect("a shell to ask for");
    assert_eq!(Some(cwd), value("HOME").map(std::path::PathBuf::from));
}

#[test]
fn the_facts_a_window_states_are_the_colours_its_pane_is_given() {
    let theme = crate::theme::active_theme();
    let facts = window_facts(&theme, Some((9, 18)));
    let stated = |color: Option<splitlane_host::Rgb>| {
        let splitlane_host::Rgb(r, g, b) = color.expect("a colour");
        ghostty::Rgb { r, g, b }
    };
    // The three colours `new_display_terminal` gives the pane's terminal,
    // which is what that terminal answers a query with.
    assert_eq!(stated(facts.foreground), ghostty_rgb(theme.foreground));
    assert_eq!(stated(facts.background), ghostty_rgb(theme.ansi_background));
    assert_eq!(stated(facts.cursor), ghostty_rgb(theme.cursor));
    assert!(facts.palette.is_empty());
    assert_eq!((facts.cell_width, facts.cell_height), (Some(9), Some(18)));
    // Before a frame has laid a cell out there is no size to state.
    let early = window_facts(&theme, None);
    assert_eq!((early.cell_width, early.cell_height), (None, None));
    assert!(early.background.is_some());
}

/// Collects what a runtime writes to its PTY, in place of one.
#[derive(Clone, Default)]
struct WrittenToPty(Arc<Mutex<Vec<u8>>>);

impl Write for WrittenToPty {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The host answers a program from an emulator of its own, and a program
/// must not be able to tell which of the two answered. This asks both the
/// same questions: a terminal built and read the way a pane that owns its
/// PTY builds and reads one - the same two functions, with the answers
/// going to a buffer in place of the PTY - and the host's engine, told
/// what a window tells the host.
#[test]
fn the_session_host_answers_queries_like_a_pane_does() {
    let size = TerminalWindowSize::new(80, 24, 9, 18);
    // What a window tells the host about itself.
    let facts = window_facts(
        &crate::theme::active_theme(),
        Some((size.cell_width, size.cell_height)),
    );
    let mut host = splitlane_host::engine::for_this_platform(80, 24)
        .expect("the session host has an engine on Linux with libghostty");
    host.set_facts(&facts);

    let (session, pending, _events) = GhosttySession::pending(size);
    let inner = &session.inner;
    let (mut terminal, _) = new_display_terminal(inner, 1000).expect("a pane's terminal");
    let written = WrittenToPty::default();
    let mut writer: Option<Box<dyn Write + Send>> = Some(Box::new(written.clone()));
    let mut marks_scanner = Osc133Scanner::default();
    let mut status = StatusTap::new(inner.program_status.clone());
    let mut service_output_tail = ServiceOutputTail::default();
    let (mut last_refresh, mut refresh_pending) = (None, false);

    let queries: &[&[u8]] = &[
        // Cursor position and status, at the start and after output.
        b"\x1b[6n",
        b"\x1b[5n",
        b"some text\r\nmore\x1b[6n",
        // Device attributes and the terminal's name.
        b"\x1b[c",
        b"\x1b[>c",
        b"\x1b[=c",
        b"\x1b[>q",
        // The keyboard protocol: asked about, switched on, asked again.
        b"\x1b[?u",
        b"\x1b[>1u\x1b[?u",
        b"\x1b[=5;1u\x1b[?u\x1b[<u",
        // Modes, known and unknown, private and not.
        b"\x1b[?1$p\x1b[?7$p\x1b[?12$p\x1b[?25$p\x1b[?1000$p\x1b[?1004$p",
        b"\x1b[?1006$p\x1b[?1049$p\x1b[?2004$p\x1b[?2026$p\x1b[?2027$p\x1b[?2031$p",
        b"\x1b[?2004h\x1b[?1049h\x1b[?2004$p\x1b[?1049$p\x1b[6n\x1b[?1049l",
        b"\x1b[4$p\x1b[20$p\x1b[99$p",
        // Sizes, in cells and in pixels.
        b"\x1b[18t",
        b"\x1b[14t",
        b"\x1b[16t\x1b[19t",
        // Colors, ended both ways, and the palette, which neither answers.
        b"\x1b]10;?\x07",
        b"\x1b]11;?\x1b\\",
        b"\x1b]12;?\x07",
        b"\x1b]4;0;?\x07\x1b]4;9;?\x07\x1b]4;16;?\x07\x1b]4;231;?\x07\x1b]4;255;?\x07",
        b"\x1b]4;256;?\x07\x1b]13;?\x07\x1b]19;?\x07",
        // The clipboard, and a string nobody knows.
        b"\x1b]52;c;?\x07",
        b"\x1bP+q544e\x1b\\\x1bP$qm\x1b\\",
        // A question inside a synchronized update.
        b"\x1b[?2026h\x1b[6n\x1b[?2026l",
        // A question behind a string that was cancelled, and behind one
        // that an escape cut short.
        b"\x1b]0;abc\x18\x1b[5n",
        b"\x1b]0;abc\x1b[6n",
        // Everything a program asks at start-up, in one write.
        b"\x1b[?u\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[6n\x1b[c",
    ];
    let mut answered = 0;
    for query in queries {
        let mut chunk = pending
            .mailbox
            .take_output_buffer()
            .expect("a buffer for the chunk");
        chunk.clear();
        chunk.extend_from_slice(query);
        process_output_batch(
            inner,
            &pending.mailbox,
            &mut terminal,
            &mut Replies::ToPty(&mut writer),
            None,
            &mut marks_scanner,
            &mut status,
            &mut service_output_tail,
            &mut last_refresh,
            &mut refresh_pending,
            chunk,
        )
        .expect("the pane reads the query");
        let from_pane = std::mem::take(
            &mut *written
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );

        let mut from_host = Vec::new();
        host.advance(query, &mut from_host);
        let show = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace('\x1b', "^");
        assert_eq!(
            show(&from_host),
            show(&from_pane),
            "the session host and a pane answer {:?} differently",
            show(query)
        );
        answered += usize::from(!from_pane.is_empty());
    }
    // Not a comparison of two silences.
    assert!(
        answered >= 15,
        "only {answered} of the queries were answered"
    );
}
