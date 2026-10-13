//! A real host on a temporary socket, real `/bin/sh` children, real clients.
//!
//! Nothing here asserts by sleeping: every wait is a poll with a deadline
//! that fails with what was seen so far.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::panic,
    reason = "test helpers fail the test by panicking, like the tests that call them"
)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use splitlane_host::protocol::{
    self, AttachArgs, AttachResult, EventBody, Hello, ReplyEnvelope, RequestEnvelope, WireError,
};
use splitlane_host::{
    ensure_running, AttachFrom, Attachment, Client, ExitStatus, HostConfig, HostError, SessionId,
    SessionInfo, SpawnSpec, StreamEvent,
};
#[cfg(target_os = "macos")]
use splitlane_host::{Facts, Rgb};

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

fn process_exists(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the process is there.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A host running on a thread of the test, on a socket of its own.
struct TestHost {
    // Field order matters: the directory is removed last.
    thread: Option<JoinHandle<Result<(), HostError>>>,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// A directory short enough for a socket path: macOS allows 104 bytes and
/// its default temporary directory uses half of them.
fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("slh")
        .tempdir_in("/tmp")
        .unwrap()
}

impl TestHost {
    fn start(configure: impl FnOnce(&mut HostConfig)) -> Self {
        let dir = short_dir();
        // One level down, so the host has a directory to create.
        let socket = dir.path().join("h").join("host.sock");
        let mut config = HostConfig::new(&socket);
        configure(&mut config);
        let thread = std::thread::spawn(move || splitlane_host::run(config));
        let host = TestHost {
            thread: Some(thread),
            socket,
            _dir: dir,
        };
        drop(host.client());
        host
    }

    fn client(&self) -> Client {
        wait_for("the host to answer", || {
            if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                panic!("the host exited before answering");
            }
            Client::connect(&self.socket).ok()
        })
    }

    /// Wait for the host to exit by itself and return how it ended.
    fn join(&mut self) -> Result<(), HostError> {
        let thread = self.thread.take().unwrap();
        wait_for("the host to exit", || thread.is_finished().then_some(()));
        thread.join().unwrap()
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if !thread.is_finished() {
            if let Ok(client) = Client::connect(&self.socket) {
                let _ = client.shutdown(true);
            }
        }
        let _ = thread.join();
    }
}

fn sh(script: &str) -> SpawnSpec {
    SpawnSpec {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        env: vec![("PATH".into(), "/bin:/usr/bin".into())],
        cwd: "/".into(),
        cols: 80,
        rows: 24,
        ..SpawnSpec::default()
    }
}

fn info(client: &Client, session: SessionId) -> SessionInfo {
    client
        .list()
        .unwrap()
        .into_iter()
        .find(|info| info.id == session)
        .unwrap()
}

/// Wait until the session's log has grown to exactly `tail` positions.
fn wait_for_tail(client: &Client, session: SessionId, tail: u64) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = info(client, session);
        if listed.log_tail_seq == tail {
            return;
        }
        if Instant::now() >= deadline || listed.log_tail_seq > tail {
            let attachment = client.attach(session, AttachFrom::Start).unwrap();
            let mut trace = Trace::starting_at(&attachment);
            while let Ok(event) = attachment.recv_timeout(Duration::from_millis(300)) {
                trace.record(event);
            }
            let text = trace.text();
            panic!(
                "the log stopped at {} of {tail} positions ({listed:?}); it ends with {:?}",
                listed.log_tail_seq,
                &text[text.len().saturating_sub(300)..]
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One position of a session's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pos {
    Byte(u8),
    Resize(u16, u16),
}

/// Everything one attachment delivered, position by position. Recording it
/// checks the one thing every stream must hold: each event starts where the
/// previous one ended, unless a gap said otherwise.
#[derive(Debug, Default)]
struct Trace {
    next: Option<u64>,
    positions: Vec<(u64, Pos)>,
    gaps: Vec<(u64, u64)>,
    /// Each preamble with the position it was sent ahead of.
    preambles: Vec<(u64, Vec<u8>)>,
    /// A gap was reported and its preamble has not arrived yet.
    preamble_due: bool,
    exit: Option<ExitStatus>,
}

impl Trace {
    fn starting_at(attachment: &Attachment) -> Self {
        Trace {
            next: Some(attachment.start_seq),
            // A stream that starts later than asked opens with one.
            preamble_due: attachment.gap,
            ..Trace::default()
        }
    }

    fn record(&mut self, event: StreamEvent) {
        assert!(self.exit.is_none(), "an event after the exit: {event:?}");
        // A preamble comes exactly where it is owed and nowhere else.
        assert_eq!(
            matches!(event, StreamEvent::Preamble(_)),
            self.preamble_due,
            "{event:?}"
        );
        self.preamble_due = false;
        match event {
            StreamEvent::Output { seq, bytes } => {
                assert_eq!(Some(seq), self.next, "output out of sequence");
                assert!(!bytes.is_empty());
                for (offset, byte) in bytes.iter().enumerate() {
                    self.positions.push((seq + offset as u64, Pos::Byte(*byte)));
                }
                self.next = Some(seq + bytes.len() as u64);
            }
            StreamEvent::Resized { seq, cols, rows } => {
                assert_eq!(Some(seq), self.next, "resize out of sequence");
                self.positions.push((seq, Pos::Resize(cols, rows)));
                self.next = Some(seq + 1);
            }
            StreamEvent::Gap {
                lost_from,
                resumes_at,
            } => {
                assert_eq!(Some(lost_from), self.next, "gap out of sequence");
                assert!(resumes_at > lost_from);
                self.gaps.push((lost_from, resumes_at));
                self.next = Some(resumes_at);
                self.preamble_due = true;
            }
            StreamEvent::Preamble(bytes) => {
                assert_eq!(bytes.first(), Some(&0x18), "a preamble opens with a cancel");
                self.preambles.push((self.next.unwrap(), bytes));
            }
            StreamEvent::Exited(status) => self.exit = Some(status),
        }
    }

    fn text(&self) -> String {
        let bytes: Vec<u8> = self
            .positions
            .iter()
            .filter_map(|(_, pos)| match pos {
                Pos::Byte(byte) => Some(*byte),
                Pos::Resize(..) => None,
            })
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Take events from `attachment` until `done` holds.
    fn follow(&mut self, attachment: &Attachment, what: &str, done: impl Fn(&Trace) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; got {:?}, exit {:?}",
                self.text(),
                self.exit
            );
            if let Ok(event) = attachment.recv_timeout(Duration::from_millis(50)) {
                self.record(event);
            }
        }
    }

    fn follow_to_exit(&mut self, attachment: &Attachment) -> ExitStatus {
        self.follow(attachment, "the session to exit", |trace| {
            trace.exit.is_some()
        });
        self.exit.unwrap()
    }
}

/// Attach from the start and collect everything up to the exit.
fn whole_session(client: &Client, session: SessionId) -> Trace {
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    assert_eq!((attachment.start_seq, attachment.gap), (0, false));
    let mut trace = Trace::starting_at(&attachment);
    trace.follow_to_exit(&attachment);
    trace
}

#[test]
fn a_child_sees_exactly_the_environment_it_was_given() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    // `env` itself, not a shell running it: a shell adds variables of its
    // own before `env` gets to print them.
    let (session, pid) = client
        .spawn(SpawnSpec {
            argv: vec!["/usr/bin/env".into()],
            env: vec![
                ("ZED".into(), "last".into()),
                ("ALPHA".into(), "two words".into()),
            ],
            cwd: "/".into(),
            cols: 80,
            rows: 24,
            ..SpawnSpec::default()
        })
        .unwrap();
    assert!(pid > 1);
    let trace = whole_session(&client, session);
    assert_eq!(
        trace.exit,
        Some(ExitStatus {
            code: Some(0),
            signal: None
        })
    );
    let text = trace.text();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["ALPHA=two words", "ZED=last"]);
}

#[test]
fn a_spawn_that_cannot_work_is_refused_with_the_reason() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let refused = |spec: SpawnSpec| {
        let error = client.spawn(spec).unwrap_err();
        assert_eq!(error.remote_code(), Some(protocol::ERR_SPAWN), "{error}");
        error.to_string()
    };
    assert!(refused(SpawnSpec::default()).contains("argv is empty"));
    assert!(refused(SpawnSpec {
        cwd: "/no/such/directory".into(),
        ..sh("true")
    })
    .contains("not a directory"));
    assert!(refused(SpawnSpec {
        argv: vec!["/no/such/program".into()],
        ..sh("true")
    })
    .contains("/no/such/program"));
    assert!(refused(SpawnSpec {
        cols: 0,
        ..sh("true")
    })
    .contains("empty"));
    assert!(client.list().unwrap().is_empty());
}

#[test]
fn input_reaches_the_child_and_its_answer_comes_back() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let (session, _) = client
        .spawn(sh(r#"printf 'ready\n'; read line; echo "got:$line""#))
        .unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the prompt", |t| t.text().contains("ready"));
    client.input(session, b"hello\n").unwrap();
    assert_eq!(trace.follow_to_exit(&attachment).code, Some(0));
    assert!(trace.text().contains("got:hello"), "{:?}", trace.text());
}

#[test]
fn a_resize_reaches_the_child_and_is_recorded_where_it_happened() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let (session, _) = client
        .spawn(sh("stty size; read a; stty size; read b"))
        .unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    assert_eq!((attachment.cols, attachment.rows), (80, 24));
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the first size", |t| {
        t.text().contains("24 80")
    });

    client.resize(session, 100, 30).unwrap();
    let listed = info(&client, session);
    assert_eq!((listed.cols, listed.rows), (100, 30));
    client.input(session, b"\n").unwrap();
    trace.follow(&attachment, "the second size", |t| {
        t.text().contains("30 100")
    });

    // The record sits after the first answer and before the second, at its
    // own position; `Trace::record` has already checked that position
    // follows the output before it without a hole.
    let resize_at = trace
        .positions
        .iter()
        .position(|(_, pos)| *pos == Pos::Resize(100, 30))
        .unwrap();
    let before: Trace = Trace {
        positions: trace.positions[..resize_at].to_vec(),
        ..Trace::default()
    };
    assert!(before.text().contains("24 80"));
    assert!(!before.text().contains("30 100"));

    assert!(client.resize(session, 0, 10).is_err());
    client.input(session, b"\n").unwrap();
    trace.follow_to_exit(&attachment);
    let error = client.resize(session, 90, 20).unwrap_err();
    assert_eq!(error.remote_code(), Some(protocol::ERR_SESSION_EXITED));
}

#[test]
fn a_late_attach_replays_the_log_and_then_follows_live() {
    let host = TestHost::start(|_| {});
    let first_client = host.client();
    let (session, _) = first_client
        .spawn(sh("echo one; read a; echo two; read b; echo three"))
        .unwrap();
    let first = first_client.attach(session, AttachFrom::Start).unwrap();
    let mut first_trace = Trace::starting_at(&first);
    first_trace.follow(&first, "one", |t| t.text().contains("one"));
    first_client.resize(session, 120, 40).unwrap();
    first_client.input(session, b"\n").unwrap();
    first_trace.follow(&first, "two", |t| t.text().contains("two"));

    // Two more clients arrive in the middle: one wants everything, one
    // continues from a position inside what was already printed.
    let late_client = host.client();
    let late = late_client.attach(session, AttachFrom::Start).unwrap();
    assert_eq!((late.start_seq, late.gap), (0, false));
    assert_eq!((late.cols, late.rows), (80, 24));
    let mut late_trace = Trace::starting_at(&late);

    let resume_at = 3;
    let resuming_client = host.client();
    let resuming = resuming_client
        .attach(session, AttachFrom::Seq(resume_at))
        .unwrap();
    assert_eq!((resuming.start_seq, resuming.gap), (resume_at, false));
    let mut resuming_trace = Trace::starting_at(&resuming);

    assert_eq!(info(&late_client, session).attached, 3);
    late_client.resize(session, 60, 20).unwrap();
    late_client.input(session, b"\n").unwrap();

    for (trace, attachment) in [
        (&mut first_trace, &first),
        (&mut late_trace, &late),
        (&mut resuming_trace, &resuming),
    ] {
        assert_eq!(trace.follow_to_exit(attachment).code, Some(0));
        assert!(trace.gaps.is_empty());
    }
    assert!(first_trace.text().contains("three"));
    assert!(first_trace
        .positions
        .iter()
        .any(|(_, pos)| *pos == Pos::Resize(120, 40)));
    assert!(first_trace
        .positions
        .iter()
        .any(|(_, pos)| *pos == Pos::Resize(60, 20)));

    // Same bytes, same resize records, same positions: nothing replayed
    // twice and nothing lost between the replay and the live part.
    assert_eq!(late_trace.positions, first_trace.positions);
    let suffix: Vec<_> = first_trace
        .positions
        .iter()
        .filter(|(seq, _)| *seq >= resume_at)
        .copied()
        .collect();
    assert_eq!(resuming_trace.positions, suffix);

    // The log is still there after the exit, for whoever comes next.
    let after = whole_session(&host.client(), session);
    assert_eq!(after.positions, first_trace.positions);
}

#[test]
fn an_attach_says_where_the_log_ended_when_it_was_made() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let (session, _) = client
        .spawn(sh("printf before; read line; printf after"))
        .unwrap();
    wait_for_tail(&client, session, 6);
    client.resize(session, 100, 30).unwrap();

    // Six bytes and a resize record: seven positions had been used.
    let late_client = host.client();
    let late = late_client.attach(session, AttachFrom::Start).unwrap();
    assert_eq!((late.start_seq, late.tail_seq), (0, Some(7)));
    assert_eq!(info(&client, session).log_tail_seq, 7);

    // It is the boundary between what is replayed and what is new: the
    // position of the first thing that happens after the attach.
    client.input(session, b"\n").unwrap();
    let mut trace = Trace::starting_at(&late);
    assert_eq!(trace.follow_to_exit(&late).code, Some(0));
    let replayed: Vec<Pos> = trace
        .positions
        .iter()
        .filter(|(seq, _)| *seq < 7)
        .map(|(_, pos)| *pos)
        .collect();
    assert_eq!(replayed.len(), 7);
    assert_eq!(replayed[6], Pos::Resize(100, 30));
    assert_eq!(trace.positions[7].0, 7);

    // An attach in the middle, and one at the very end of an ended session.
    let resumed = host.client().attach(session, AttachFrom::Seq(3)).unwrap();
    let ended = info(&client, session).log_tail_seq;
    assert_eq!((resumed.start_seq, resumed.tail_seq), (3, Some(ended)));
}

/// A terminal that is not told its input is UTF-8 erases one byte for a
/// backspace, and leaves the rest of a multi-byte character on the line.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_session_terminal_treats_its_input_as_utf8() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let (session, _) = client.spawn(sh("stty -a")).unwrap();
    let text = whole_session(&client, session).text();
    let flags: Vec<&str> = text.split_whitespace().collect();
    assert!(
        flags.contains(&"iutf8") && !flags.contains(&"-iutf8"),
        "{text}"
    );
}

#[test]
fn a_detached_stream_can_be_resumed_where_it_stopped() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let (session, _) = client
        .spawn(sh("echo one; read a; echo two; read b"))
        .unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "one", |t| t.text().contains("one"));
    assert!(matches!(
        client.attach(session, AttachFrom::Start),
        Err(e) if e.remote_code() == Some(protocol::ERR_ALREADY_ATTACHED)
    ));

    client.detach(session).unwrap();
    // Whatever was queued before the detach still belongs to the stream.
    for event in attachment {
        trace.record(event);
    }
    assert_eq!(info(&client, session).attached, 0);
    client.input(session, b"\n").unwrap();

    let resumed = client
        .attach(session, AttachFrom::Seq(trace.next.unwrap()))
        .unwrap();
    assert_eq!(Some(resumed.start_seq), trace.next);
    trace.follow(&resumed, "two", |t| t.text().contains("two"));
    client.input(session, b"\n").unwrap();
    trace.follow_to_exit(&resumed);
    assert_eq!(trace.text().matches("two").count(), 1);
}

#[test]
fn a_dropped_attachment_lets_the_session_be_attached_again() {
    let host = TestHost::start(|_| {});
    let client = host.client();

    // A quiet session: nothing arrives after the drop to make the client
    // notice, so the drop itself has to release the stream.
    let (quiet, _) = client.spawn(sh("echo ready; read a")).unwrap();
    let attachment = client.attach(quiet, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "ready", |t| t.text().contains("ready"));
    drop(attachment);
    let again = client.attach(quiet, AttachFrom::Start).unwrap();
    wait_for("the first stream to be gone", || {
        (info(&client, quiet).attached == 1).then_some(())
    });
    client.input(quiet, b"\n").unwrap();
    let mut replay = Trace::starting_at(&again);
    replay.follow_to_exit(&again);
    assert!(replay.text().contains("ready"));

    // A session that prints without pause: frames of the dropped stream are
    // still arriving when the next attach is registered, and none of them
    // may be taken for part of the new stream. `Trace` fails on the first
    // event that does not continue where the previous one ended.
    let (busy, _) = client
        .spawn(sh("while :; do echo a line of output; done"))
        .unwrap();
    for _ in 0..20 {
        let attachment = client.attach(busy, AttachFrom::Start).unwrap();
        let mut trace = Trace::starting_at(&attachment);
        let seen = trace.positions.len();
        trace.follow(&attachment, "output", |t| t.positions.len() > seen);
    }
    client.kill(busy).unwrap();
}

/// What `count` lines of the loop below look like in the log.
fn numbered_lines(count: usize) -> String {
    (0..count)
        .map(|i| format!("line {i} of output\n"))
        .collect()
}

/// Output processing is switched off first, so the bytes logged are the
/// bytes written. With it on, the terminal turns each newline into a
/// carriage return and a newline, and macOS now and then emits a second
/// carriage return when its output queue fills in the middle of that pair:
/// harmless on a screen, but one byte too many for a test that counts.
fn print_numbered_lines(count: usize) -> String {
    format!(
        "stty -opost; i=0; while [ $i -lt {count} ]; do echo \"line $i of output\"; i=$((i+1)); done; read hold"
    )
}

#[test]
fn a_late_attach_to_a_trimmed_log_starts_at_its_head_and_says_so() {
    let host = TestHost::start(|config| config.log_capacity_bytes = 4096);
    let client = host.client();
    let expected = numbered_lines(2000);
    let (session, _) = client.spawn(sh(&print_numbered_lines(2000))).unwrap();
    wait_for_tail(&client, session, expected.len() as u64);
    let listed = info(&client, session);
    assert!(listed.log_bytes <= 4096);
    assert!(listed.log_head_seq > 0);
    assert_eq!(listed.exited, None);

    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    assert!(attachment.gap);
    assert_eq!(attachment.start_seq, listed.log_head_seq);
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the tail of the log", |t| {
        t.next == Some(expected.len() as u64)
    });
    assert_eq!(trace.text(), &expected[listed.log_head_seq as usize..]);

    // A position that is still in the log is served without a gap.
    let other = host.client();
    let exact = other
        .attach(session, AttachFrom::Seq(listed.log_head_seq + 10))
        .unwrap();
    assert_eq!(
        (exact.start_seq, exact.gap),
        (listed.log_head_seq + 10, false)
    );
}

fn visible(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\x1b', "^")
}

#[test]
fn a_late_attach_to_a_trimmed_log_is_given_the_modes_before_the_output() {
    let host = TestHost::start(|config| config.log_capacity_bytes = 4096);
    let client = host.client();
    // Switched on once, at the start, and long gone from the log by the
    // time anybody attaches.
    let setup = "\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?25l\x1b[5 q\x1b]2;a title\x07";
    let script = format!(
        "printf '\\033[?1049h\\033[?2004h\\033[?1000h\\033[?1006h\\033[?25l\\033[5 q\\033]2;a title\\007'; {}",
        print_numbered_lines(2000)
    );
    let expected = format!("{setup}{}", numbered_lines(2000));
    let (session, _) = client.spawn(sh(&script)).unwrap();
    wait_for_tail(&client, session, expected.len() as u64);
    let listed = info(&client, session);
    assert!(listed.log_head_seq > setup.len() as u64);

    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    assert!(attachment.gap && !attachment.torn);
    assert_eq!(attachment.start_seq, listed.log_head_seq);
    // `Trace` insists that the preamble is the first event and the only
    // one of its kind.
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the tail of the log", |t| {
        t.next == Some(expected.len() as u64)
    });
    assert_eq!(trace.text(), &expected[listed.log_head_seq as usize..]);
    let [(at, preamble)] = trace.preambles.as_slice() else {
        panic!("one preamble expected, got {:?}", trace.preambles);
    };
    assert_eq!(*at, listed.log_head_seq);
    let preamble = visible(preamble);
    for restored in [
        "^[?1049h",
        "^[?2004h",
        "^[?1000h",
        "^[?1006h",
        "^[?25l",
        "^[5 q",
        "^]2;a title^\\",
    ] {
        assert!(
            preamble.contains(restored),
            "{restored} is not in {preamble}"
        );
    }

    // A client that asks for a position the log still has was not cut off
    // from anything and is sent no preamble: `Trace` would refuse one.
    let resuming = host.client();
    let exact = resuming
        .attach(session, AttachFrom::Seq(listed.log_head_seq))
        .unwrap();
    assert!(!exact.gap && !exact.torn);
    let mut resumed = Trace::starting_at(&exact);
    resumed.follow(&exact, "the tail of the log", |t| {
        t.next == Some(expected.len() as u64)
    });
    assert!(resumed.preambles.is_empty());
}

#[test]
fn a_log_cut_through_a_sequence_says_so_and_still_gives_the_modes() {
    let host = TestHost::start(|config| config.log_capacity_bytes = 1024);
    let client = host.client();
    // One title of 6000 bytes: nowhere in it may the log be cut, and it is
    // several times the log.
    let (session, _) = client
        .spawn(sh(
            "stty -opost; printf '\\033[?2004h\\033]2;'; i=0; while [ $i -lt 100 ]; do printf '%060d' 0; i=$((i+1)); done; read hold",
        ))
        .unwrap();
    let total = "\x1b[?2004h\x1b]2;".len() as u64 + 6000;
    wait_for_tail(&client, session, total);
    let listed = info(&client, session);
    assert!(listed.log_head_seq > 0);
    assert!(listed.log_bytes <= 2 * 1024);

    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    assert!(attachment.gap && attachment.torn);
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the tail of the log", |t| {
        t.next == Some(total)
    });
    assert!(visible(&trace.preambles[0].1).contains("^[?2004h"));
    assert!(trace.text().bytes().all(|byte| byte == b'0'));
}

#[test]
fn a_redraw_resizes_the_terminal_by_a_row_and_back() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    // The shell reports each size change it is signalled about, with the
    // size it finds then.
    let (session, _) = client
        .spawn(sh(
            "trap 'echo \"winch $(stty size)\"' WINCH; sleep 1000 & echo ready; while :; do wait $!; done",
        ))
        .unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the shell to be ready", |t| {
        t.text().contains("ready")
    });
    let resizes = |trace: &Trace| -> Vec<(u16, u16)> {
        trace
            .positions
            .iter()
            .filter_map(|(_, pos)| match pos {
                Pos::Resize(cols, rows) => Some((*cols, *rows)),
                Pos::Byte(_) => None,
            })
            .collect()
    };
    let winches = |trace: &Trace| -> Vec<String> {
        trace
            .text()
            .lines()
            .filter(|line| line.starts_with("winch ") && line.ends_with('0'))
            .map(str::to_owned)
            .collect()
    };

    client.redraw(session).unwrap();
    trace.follow(&attachment, "both resizes and both signals", |t| {
        resizes(t).len() == 2 && winches(t).len() == 2
    });
    // Two records like any other resize, and the size that was there
    // before is the size that is there after.
    assert_eq!(resizes(&trace), [(80, 23), (80, 24)]);
    assert_eq!(winches(&trace)[1], "winch 24 80");
    let listed = info(&client, session);
    assert_eq!((listed.cols, listed.rows), (80, 24));

    // A real resize that lands between the two steps is what the redraw
    // comes back to, not the size it started from.
    client.redraw(session).unwrap();
    client.resize(session, 100, 30).unwrap();
    trace.follow(&attachment, "the redraw to finish", |t| {
        resizes(t).len() == 5 && winches(t).last().is_some_and(|line| line == "winch 30 100")
    });
    assert_eq!(resizes(&trace)[2..], [(80, 23), (100, 30), (100, 30)]);
    let listed = info(&client, session);
    assert_eq!((listed.cols, listed.rows), (100, 30));

    client.kill(session).unwrap();
    trace.follow_to_exit(&attachment);
    let error = client.redraw(session).unwrap_err();
    assert_eq!(error.remote_code(), Some(protocol::ERR_SESSION_EXITED));
    let error = client.redraw(SessionId::new_random()).unwrap_err();
    assert_eq!(error.remote_code(), Some(protocol::ERR_NO_SESSION));
}

/// What `od` prints for `bytes` once the blanks are taken out.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A shell command that reads `count` bytes of input and prints them as
/// hexadecimal digits. The terminal must be in raw mode: each byte is then
/// one read, so exactly `count` are consumed.
fn read_hex(count: usize) -> String {
    format!("dd bs=1 count={count} 2>/dev/null | od -An -tx1 | tr -d ' \\n'")
}

// The three tests below need an engine, and the host has one on macOS only
// so far.

/// A program asks whether its status reports are read and, so as not to
/// wait, what the terminal is; the first answer to arrive decides. The host
/// has to say yes before its emulator says anything.
#[test]
fn a_program_asking_whether_its_status_is_read_is_told_so_first() {
    use splitlane_host::program_status;

    let host = TestHost::start(|_| {});
    let client = host.client();
    let answer = program_status::reply(false);
    let script = format!(
        "stty raw -echo; printf '\\033]7501;?\\033\\\\\\033[c'; {}; printf ' end'",
        read_hex(answer.len())
    );
    let (session, _) = client.spawn(sh(&script)).unwrap();
    let exited = wait_for("the child to get its answer", || {
        info(&client, session).exited
    });
    assert_eq!(exited.code, Some(0));
    let trace = whole_session(&client, session);
    assert_eq!(
        trace.text(),
        format!("\x1b]7501;?\x1b\\\x1b[c{} end", hex(answer))
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_program_gets_its_answers_with_nobody_attached() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    // Where is the cursor, is the keyboard protocol supported, what are
    // you. The second goes unanswered, as it does in a pane, and the
    // program reads exactly the two answers that are left: one more or one
    // fewer and the digits printed would be different ones.
    let answers = b"\x1b[1;3R\x1b[?6c";
    let script = format!(
        "stty raw -echo; printf 'ab\\033[6n\\033[?u\\033[c'; {}; printf ' end'",
        read_hex(answers.len())
    );
    let (session, _) = client.spawn(sh(&script)).unwrap();
    let exited = wait_for("the child to get its answers", || {
        info(&client, session).exited
    });
    assert_eq!(exited.code, Some(0));
    // Nobody followed the session while it ran.
    let trace = whole_session(&client, session);
    assert_eq!(
        trace.text(),
        format!("ab\x1b[6n\x1b[?u\x1b[c{} end", hex(answers))
    );
    assert_eq!(info(&client, session).dropped_replies, 0);
}

#[cfg(target_os = "macos")]
#[test]
fn questions_about_the_window_are_answered_once_a_client_has_told_the_host() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let background = b"\x1b]11;rgb:1212/3434/5656\x07";
    let pixels = b"\x1b[4;432;720t";
    let resized = b"\x1b[4;600;1000t";
    // Three rounds, each started by a byte of input: before any facts,
    // after them, and after a resize that came with pixels. In the first
    // the colour goes unanswered and only the question after it is.
    let script = format!(
        "stty raw -echo; printf '\\033]11;?\\007\\033[14t\\033[c'; {}; printf ' first '; \
         dd bs=1 count=1 2>/dev/null >/dev/null; printf '\\033]11;?\\007\\033[14t'; {}; printf ' second '; \
         dd bs=1 count=1 2>/dev/null >/dev/null; printf '\\033[14t'; {}; printf ' third'",
        read_hex(5),
        read_hex(background.len() + pixels.len()),
        read_hex(resized.len()),
    );
    let (session, _) = client.spawn(sh(&script)).unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the first round", |t| {
        t.text().contains(" first ")
    });
    assert!(trace
        .text()
        .contains(&format!("{} first ", hex(b"\x1b[?6c"))));

    client
        .set_facts(
            session,
            &Facts {
                background: Some(Rgb(0x12, 0x34, 0x56)),
                cell_width: Some(9),
                cell_height: Some(18),
                ..Facts::default()
            },
        )
        .unwrap();
    client.input(session, b"g").unwrap();
    trace.follow(&attachment, "the second round", |t| {
        t.text().contains(" second ")
    });
    let mut both = background.to_vec();
    both.extend_from_slice(pixels);
    assert!(trace.text().contains(&format!("{} second ", hex(&both))));

    client
        .resize_with_pixels(session, 100, 30, 1000, 600)
        .unwrap();
    client.input(session, b"g").unwrap();
    assert_eq!(trace.follow_to_exit(&attachment).code, Some(0));
    assert!(trace.text().ends_with(&format!("{} third", hex(resized))));

    // Facts given with the spawn are there for the program's first
    // question.
    let script = format!(
        "stty raw -echo; printf '\\033]10;?\\033\\\\'; {}",
        read_hex(25)
    );
    let (session, _) = client
        .spawn(SpawnSpec {
            facts: Some(Facts {
                foreground: Some(Rgb(0xab, 0xcd, 0xef)),
                ..Facts::default()
            }),
            ..sh(&script)
        })
        .unwrap();
    let trace = whole_session(&client, session);
    assert!(trace
        .text()
        .ends_with(&hex(b"\x1b]10;rgb:abab/cdcd/efef\x1b\\")));
    // Facts for a session that is over, or was never there.
    client.set_facts(session, &Facts::default()).unwrap();
    let error = client
        .set_facts(SessionId::new_random(), &Facts::default())
        .unwrap_err();
    assert_eq!(error.remote_code(), Some(protocol::ERR_NO_SESSION));
}

#[cfg(target_os = "macos")]
#[test]
fn answers_a_program_does_not_read_are_dropped_and_hold_nothing_back() {
    const QUESTIONS: u64 = 4 * 1024 * 1024;
    let host = TestHost::start(|_| {});
    let client = host.client();
    // A million questions and not one read. The answers fill the PTY's
    // input, then the queue to it, and from there on have nowhere to go.
    let (session, _) = client
        .spawn(sh(&format!(
            "stty raw -echo; yes \"$(printf '\\033[c')\" | head -c {QUESTIONS}; printf DONE; sleep 300"
        )))
        .unwrap();
    // Every byte the program printed is in the log: the thread reading its
    // output never waited for room to answer.
    wait_for_tail(&client, session, QUESTIONS + 4);
    let listed = info(&client, session);
    assert!(listed.dropped_replies > 0, "{listed:?}");
    assert_eq!(listed.exited, None);
    // And the session still does what it is told.
    client.kill(session).unwrap();
    wait_for("the session to end", || info(&client, session).exited);
}

/// A connection driven by hand, for what [`Client`] will not do: say the
/// wrong thing, or say nothing.
struct RawClient {
    stream: UnixStream,
}

impl RawClient {
    fn open(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        RawClient { stream }
    }

    fn connect(socket: &Path) -> Self {
        let mut raw = Self::open(socket);
        protocol::write_json(&mut raw.stream, protocol::KIND_HELLO, &Hello::local("raw")).unwrap();
        let frame = protocol::read_frame(&mut raw.stream).unwrap();
        assert_eq!(frame.kind, protocol::KIND_HELLO);
        raw
    }

    /// The error frame the host sends before it closes the connection.
    fn refusal(&mut self) -> WireError {
        let frame = protocol::read_frame(&mut self.stream).unwrap();
        assert_eq!(frame.kind, protocol::KIND_ERROR);
        let error = protocol::decode_json(&frame.body).unwrap();
        // Closed, or reset when the host hung up on bytes it never read.
        assert!(protocol::read_frame(&mut self.stream).is_err());
        error
    }
}

#[test]
fn a_client_that_stops_reading_does_not_hold_the_session_back() {
    const TOTAL: u64 = 2 * 1024 * 1024;
    let host = TestHost::start(|config| config.log_capacity_bytes = 128 * 1024);
    let client = host.client();
    let (session, _) = client
        .spawn(sh(
            "read go; dd if=/dev/zero bs=65536 count=32 2>/dev/null | tr '\\0' x",
        ))
        .unwrap();

    // Attached, and then silent: nothing is read from this socket until the
    // child has long finished.
    let mut stalled = RawClient::connect(&host.socket);
    protocol::write_json(
        &mut stalled.stream,
        protocol::KIND_REQUEST,
        &RequestEnvelope {
            id: 1,
            op: protocol::OP_ATTACH.into(),
            args: serde_json::to_value(AttachArgs {
                session,
                from_seq: None,
            })
            .unwrap(),
        },
    )
    .unwrap();
    wait_for("the silent client to be attached", || {
        (info(&client, session).attached == 1).then_some(())
    });

    client.input(session, b"\n").unwrap();
    // The echoed newline comes first, as two bytes.
    let total = TOTAL + 2;
    let exited = wait_for("the child to finish", || info(&client, session).exited);
    assert_eq!(exited.code, Some(0));
    let listed = info(&client, session);
    assert_eq!(listed.log_tail_seq, total);
    assert!(listed.log_head_seq > 0);

    // Now it reads. It gets what was already in the socket, is told what
    // it missed, and is brought to the end.
    let reply = protocol::read_frame(&mut stalled.stream).unwrap();
    assert_eq!(reply.kind, protocol::KIND_REPLY);
    let reply: ReplyEnvelope = protocol::decode_json(&reply.body).unwrap();
    let attached: AttachResult = serde_json::from_value(reply.result).unwrap();
    assert_eq!((attached.start_seq, attached.gap), (0, false));

    let mut next = 0u64;
    let mut lost = 0u64;
    let mut preamble_due = false;
    let status = loop {
        let frame = protocol::read_frame(&mut stalled.stream).unwrap();
        // Every gap is followed by the modes to resume in, before anything
        // else.
        assert_eq!(frame.kind == protocol::KIND_PREAMBLE, preamble_due);
        preamble_due = false;
        match frame.kind {
            protocol::KIND_PREAMBLE => {
                let (id, bytes) = protocol::decode_preamble(&frame.body).unwrap();
                assert_eq!(id, session);
                assert!(bytes.starts_with(b"\x18\x1b[?2026l"));
            }
            protocol::KIND_OUTPUT => {
                let output = protocol::decode_output(&frame.body).unwrap();
                assert_eq!(output.seq, next);
                next += output.bytes.len() as u64;
            }
            protocol::KIND_EVENT => {
                let event: EventBody = protocol::decode_json(&frame.body).unwrap();
                match event.event.as_str() {
                    protocol::EVENT_GAP => {
                        assert_eq!(event.lost_from, Some(next));
                        let resumes_at = event.resumes_at.unwrap();
                        lost += resumes_at - next;
                        next = resumes_at;
                        preamble_due = true;
                    }
                    protocol::EVENT_EXITED => break event.code,
                    other => panic!("unexpected event {other}"),
                }
            }
            other => panic!("unexpected frame kind {other}"),
        }
    };
    assert_eq!(status, Some(0));
    assert_eq!(next, total);
    assert!(lost > 0, "a 128 KiB log cannot have kept 2 MiB");
}

#[test]
fn all_logs_together_stay_within_the_total_and_the_largest_pays() {
    let host = TestHost::start(|config| {
        config.log_capacity_bytes = 1024 * 1024;
        config.total_log_capacity_bytes = 64 * 1024;
    });
    let client = host.client();
    let (quiet, _) = client.spawn(sh("echo quiet; read hold")).unwrap();
    let (loud, _) = client.spawn(sh(&print_numbered_lines(10_000))).unwrap();
    let loud_total = numbered_lines(10_000).len() as u64;
    wait_for_tail(&client, loud, loud_total);
    let (quiet, loud) = (info(&client, quiet), info(&client, loud));
    assert!(quiet.log_bytes + loud.log_bytes <= 64 * 1024);
    assert!(loud.log_head_seq > 0);
    // Under its own capacity the whole time: only the total cut it.
    assert!(loud_total < 1024 * 1024);
    assert_eq!(quiet.log_head_seq, 0);
}

#[test]
fn an_exit_is_reported_after_the_last_output_and_kept_until_forgotten() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    // Several times over: output written right before the exit is the part
    // that a race would lose.
    for round in 0..10 {
        let (session, _) = client
            .spawn(sh(&format!("printf 'last words {round}'; exit 7")))
            .unwrap();
        let trace = whole_session(&client, session);
        assert_eq!(
            trace.exit,
            Some(ExitStatus {
                code: Some(7),
                signal: None
            })
        );
        assert_eq!(trace.text(), format!("last words {round}"));
        client.forget(session).unwrap();
    }

    let (session, _) = client.spawn(sh("printf dying; kill -9 $$")).unwrap();
    let trace = whole_session(&client, session);
    assert_eq!(
        trace.exit,
        Some(ExitStatus {
            code: None,
            signal: Some(libc::SIGKILL)
        })
    );
    assert_eq!(trace.text(), "dying");

    // Still listed, with its log, for a client that was not there.
    let listed = info(&host.client(), session);
    assert_eq!(listed.exited.unwrap().signal, Some(libc::SIGKILL));
    assert_eq!(listed.log_tail_seq, 5);
    assert_eq!(whole_session(&host.client(), session).text(), "dying");
    // Input for it goes nowhere and breaks nothing.
    client.input(session, b"anybody?\n").unwrap();

    client.forget(session).unwrap();
    assert!(client.list().unwrap().is_empty());
    let error = client.forget(session).unwrap_err();
    assert_eq!(error.remote_code(), Some(protocol::ERR_NO_SESSION));

    let (running, _) = client.spawn(sh("read hold")).unwrap();
    let error = client.forget(running).unwrap_err();
    assert_eq!(error.remote_code(), Some(protocol::ERR_SESSION_LIVE));
}

#[test]
fn kill_ends_the_whole_process_group() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let (session, pid) = client
        .spawn(sh("sleep 300 & echo \"child=$!\"; wait"))
        .unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the background pid", |t| {
        t.text().contains("\r\n")
    });
    let grandchild: u32 = trace
        .text()
        .trim()
        .strip_prefix("child=")
        .unwrap()
        .parse()
        .unwrap();
    assert!(process_exists(pid) && process_exists(grandchild));

    client.kill(session).unwrap();
    // Hung up on first, as by a terminal that closed, and that is what a
    // program with no handler of its own dies of.
    assert_eq!(trace.follow_to_exit(&attachment).signal, Some(libc::SIGHUP));
    wait_for("both processes to be gone", || {
        (!process_exists(pid) && !process_exists(grandchild)).then_some(())
    });
    // Killing what is already dead is not an error.
    client.kill(session).unwrap();
}

/// An interactive shell ignores SIGTERM and saves its history when its
/// terminal hangs up. Killed with SIGTERM alone it would be left for the
/// SIGKILL and save nothing.
#[test]
fn kill_hangs_up_first_so_a_shell_can_save_what_it_has() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    let dir = short_dir();
    let saved = dir.path().join("saved");
    let (session, _) = client
        .spawn(sh(&format!(
            "trap '' TERM; trap 'echo history > {}; exit 0' HUP; echo ready; while :; do sleep 0.05; done",
            saved.display()
        )))
        .unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the traps to be set", |t| {
        t.text().contains("ready")
    });

    client.kill(session).unwrap();
    assert_eq!(trace.follow_to_exit(&attachment).code, Some(0));
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "history\n");
}

#[test]
fn kill_reaches_a_process_that_ignores_the_polite_request() {
    let host = TestHost::start(|_| {});
    let client = host.client();
    // The shell dies of the hangup at once. The `sleep` inherits the
    // ignored signals and is left for the SIGKILL that follows.
    let (session, pid) = client
        .spawn(sh(
            "(trap '' HUP TERM; sleep 300 & echo \"child=$!\"; wait) & wait",
        ))
        .unwrap();
    let attachment = client.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "the background pid", |t| {
        t.text().contains("\r\n")
    });
    let stubborn: u32 = trace
        .text()
        .trim()
        .strip_prefix("child=")
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(stubborn, pid);

    client.kill(session).unwrap();
    wait_for("both processes to be gone", || {
        (!process_exists(pid) && !process_exists(stubborn)).then_some(())
    });
}

#[test]
fn an_incompatible_peer_is_told_which_side_is_too_old() {
    let host = TestHost::start(|_| {});
    let connect = |contract, min_contract| {
        Client::connect_with(
            &host.socket,
            &Hello {
                contract,
                min_contract,
                version: "test".into(),
                pid: None,
            },
            DEADLINE,
        )
    };

    let Err(HostError::Incompatible(message)) = connect(0, 0) else {
        panic!("a client older than the host accepts must be refused");
    };
    assert!(message.contains("client is too old"), "{message}");

    // The refusal is the host's own, not only the client's check: a raw
    // connection gets it as an error frame.
    let mut raw = RawClient::open(&host.socket);
    protocol::write_json(
        &mut raw.stream,
        protocol::KIND_HELLO,
        &Hello {
            contract: protocol::CONTRACT + 5,
            min_contract: protocol::CONTRACT + 1,
            version: "from the future".into(),
            pid: None,
        },
    )
    .unwrap();
    let refusal = raw.refusal();
    assert_eq!(refusal.code, protocol::ERR_INCOMPATIBLE);
    assert!(
        refusal.message.contains("session host is too old"),
        "{}",
        refusal.message
    );

    // A newer client that still accepts this contract is welcome, and a
    // different release version is no obstacle at all.
    let newer = connect(protocol::CONTRACT + 5, protocol::MIN_CONTRACT).unwrap();
    assert_eq!(newer.contract(), protocol::CONTRACT);
    assert_eq!(newer.host().pid, Some(std::process::id()));
}

#[test]
fn a_peer_that_breaks_the_framing_is_dropped_and_nobody_else_notices() {
    let host = TestHost::start(|_| {});
    let bystander = host.client();
    let (session, _) = bystander.spawn(sh("echo one; read a; echo two")).unwrap();
    let attachment = bystander.attach(session, AttachFrom::Start).unwrap();
    let mut trace = Trace::starting_at(&attachment);
    trace.follow(&attachment, "one", |t| t.text().contains("one"));

    // Not a frame at all: the first four bytes read as a length of about
    // half a gigabyte.
    let mut garbage = RawClient::open(&host.socket);
    garbage.stream.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(garbage.refusal().code, protocol::ERR_PROTOCOL);

    // A proper hello, then a frame over the limit. Only its header is ever
    // sent: the host must refuse it without waiting for the body.
    let mut oversized = RawClient::connect(&host.socket);
    let mut header = ((protocol::MAX_FRAME_LEN + 1) as u32)
        .to_le_bytes()
        .to_vec();
    header.push(protocol::KIND_INPUT);
    oversized.stream.write_all(&header).unwrap();
    let refusal = oversized.refusal();
    assert_eq!(refusal.code, protocol::ERR_PROTOCOL);
    assert!(refusal.message.contains("exceeds"), "{}", refusal.message);

    // Something other than a hello first.
    let mut rude = RawClient::open(&host.socket);
    protocol::write_input(&mut rude.stream, session, b"rm -rf /\n").unwrap();
    assert_eq!(rude.refusal().code, protocol::ERR_PROTOCOL);

    // A request that is not JSON.
    let mut confused = RawClient::connect(&host.socket);
    protocol::write_frame(&mut confused.stream, protocol::KIND_REQUEST, &[b"{nope"]).unwrap();
    assert_eq!(confused.refusal().code, protocol::ERR_PROTOCOL);

    // A request from a later contract and a frame kind from one: both are
    // answered or skipped, and the connection lives on.
    let mut curious = RawClient::connect(&host.socket);
    protocol::write_frame(&mut curious.stream, 200, &[b"whatever"]).unwrap();
    protocol::write_json(
        &mut curious.stream,
        protocol::KIND_REQUEST,
        &RequestEnvelope {
            id: 9,
            op: "teleport".into(),
            args: serde_json::Value::Null,
        },
    )
    .unwrap();
    let reply = protocol::read_frame(&mut curious.stream).unwrap();
    let reply: ReplyEnvelope = protocol::decode_json(&reply.body).unwrap();
    assert_eq!((reply.id, reply.ok), (9, false));
    assert_eq!(reply.error.unwrap().code, protocol::ERR_UNKNOWN_OP);

    // The bystander's session never saw any of it.
    bystander.input(session, b"\n").unwrap();
    assert_eq!(trace.follow_to_exit(&attachment).code, Some(0));
    assert_eq!(trace.text(), "one\r\n\r\ntwo\r\n");
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn the_socket_is_private_and_belongs_to_one_host() {
    let mut host = TestHost::start(|_| {});
    assert_eq!(mode_of(&host.socket), 0o600);
    assert_eq!(mode_of(host.socket.parent().unwrap()), 0o700);

    // A second host on the same socket refuses to start and leaves the
    // first one's socket alone.
    let second = splitlane_host::run(HostConfig::new(&host.socket));
    assert!(
        matches!(&second, Err(HostError::AlreadyRunning(path)) if path == &host.socket),
        "{second:?}"
    );
    let client = host.client();
    assert!(client.list().unwrap().is_empty());

    client.shutdown(false).unwrap();
    host.join().unwrap();
    assert!(!host.socket.exists());
    assert!(matches!(
        Client::connect(&host.socket),
        Err(HostError::NotRunning)
    ));
}

#[test]
fn a_socket_file_left_by_a_dead_host_is_replaced() {
    let dir = short_dir();
    let socket = dir.path().join("host.sock");
    // Bound and dropped: the file stays, and nothing listens on it.
    drop(UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    // Asked until the answer is the lasting one. Other tests of this binary
    // start children all the time, and a child forked while the listener
    // above was open keeps it open until it execs: a connect in that moment
    // is accepted by nobody and times out instead of being refused.
    wait_for("the dead socket to refuse connections", || {
        matches!(Client::connect(&socket), Err(HostError::NotRunning)).then_some(())
    });

    let config = HostConfig::new(&socket);
    let thread = std::thread::spawn(move || splitlane_host::run(config));
    let client = wait_for("the host to answer", || Client::connect(&socket).ok());
    client.shutdown(false).unwrap();
    thread.join().unwrap().unwrap();
}

#[test]
fn a_path_that_is_not_a_socket_is_left_alone() {
    let dir = short_dir();
    let socket = dir.path().join("host.sock");
    std::fs::write(&socket, "somebody's file").unwrap();
    let outcome = splitlane_host::run(HostConfig::new(&socket));
    assert!(matches!(outcome, Err(HostError::Config(_))), "{outcome:?}");
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "somebody's file");

    let too_long = dir.path().join("x".repeat(120));
    let outcome = splitlane_host::run(HostConfig::new(too_long));
    assert!(matches!(outcome, Err(HostError::Config(_))), "{outcome:?}");
}

#[test]
fn a_directory_the_caller_chose_is_used_as_it_is_or_not_at_all() {
    let dir = short_dir();
    let open_dir = dir.path().join("open");
    std::fs::create_dir(&open_dir).unwrap();
    std::fs::set_permissions(&open_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let mut config = HostConfig::new(open_dir.join("host.sock"));
    config.owns_socket_dir = false;
    let outcome = splitlane_host::run(config);
    assert!(matches!(outcome, Err(HostError::Config(_))), "{outcome:?}");
    assert_eq!(mode_of(&open_dir), 0o777);

    let mut config = HostConfig::new(dir.path().join("missing").join("host.sock"));
    config.owns_socket_dir = false;
    assert!(matches!(
        splitlane_host::run(config),
        Err(HostError::Config(_))
    ));
}

#[test]
fn shutdown_is_refused_while_a_session_runs_unless_forced() {
    let mut host = TestHost::start(|_| {});
    let client = host.client();
    let (_, pid) = client.spawn(sh("sleep 300")).unwrap();
    let error = client.shutdown(false).unwrap_err();
    assert_eq!(error.remote_code(), Some(protocol::ERR_LIVE_SESSIONS));
    assert!(process_exists(pid));

    client.shutdown(true).unwrap();
    host.join().unwrap();
    wait_for("the session's child to be gone", || {
        (!process_exists(pid)).then_some(())
    });
}

#[test]
fn an_idle_host_exits_after_the_grace_period() {
    let grace = Duration::from_millis(300);
    let mut host = TestHost::start(|config| config.idle_grace = grace);
    let started = Instant::now();
    host.join().unwrap();
    assert!(started.elapsed() >= grace);
    assert!(!host.socket.exists());
}

#[test]
fn a_host_with_a_running_session_or_a_client_does_not_exit() {
    let grace = Duration::from_millis(200);
    let mut host = TestHost::start(|config| config.idle_grace = grace);
    let client = host.client();
    let (session, _) = client.spawn(sh("read hold")).unwrap();
    drop(client);

    // No client for several grace periods; the session alone keeps the
    // host. This is the one place where time passing is the thing tested.
    std::thread::sleep(grace * 5);
    assert!(!host.thread.as_ref().unwrap().is_finished());

    // A connected client keeps it as well, with the session gone.
    let client = host.client();
    client.kill(session).unwrap();
    wait_for("the session to end", || info(&client, session).exited);
    std::thread::sleep(grace * 5);
    assert!(!host.thread.as_ref().unwrap().is_finished());

    // The exited session was never forgotten. With nobody connected it
    // does not keep the host.
    drop(client);
    host.join().unwrap();
}

/// Not a test of its own: when the test binary is started with a socket
/// path among its arguments, this is the host process that
/// `ensure_running` launches below. In a normal run there is no such
/// argument and it does nothing.
#[test]
fn host_process_entry() {
    let Some(socket) = std::env::args().find(|arg| arg.ends_with("/race.sock")) else {
        return;
    };
    // What the real host process does first. Here it also matters for the
    // test itself: eight callers fork at once, and a host that kept another
    // caller's descriptors would keep that caller waiting.
    splitlane_host::close_inherited_descriptors();
    let mut config = HostConfig::new(socket);
    config.idle_grace = Duration::from_secs(2);
    let outcome = splitlane_host::run(config);
    std::process::exit(if outcome.is_ok() { 0 } else { 3 });
}

/// Pids of the processes started with `socket` among their arguments.
///
/// Read from `/proc` where there is one, so the test asks for no program
/// that a minimal Linux image lacks; macOS has no `/proc` and ships `pgrep`.
#[cfg(target_os = "linux")]
fn host_processes(socket: &Path) -> Vec<u32> {
    let wanted = socket.to_str().unwrap().as_bytes();
    let mut pids: Vec<u32> = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|line| line.split(|byte| *byte == 0).any(|arg| arg == wanted))
        })
        .collect();
    pids.sort_unstable();
    pids
}

#[cfg(not(target_os = "linux"))]
fn host_processes(socket: &Path) -> Vec<u32> {
    let output = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(socket)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

#[test]
fn callers_racing_to_start_a_host_end_up_with_one() {
    let dir = short_dir();
    let socket = dir.path().join("race.sock");
    let exe = std::env::current_exe().unwrap();
    let callers: Vec<_> = (0..8)
        .map(|_| {
            let (exe, socket) = (exe.clone(), socket.clone());
            std::thread::spawn(move || {
                let socket_arg = socket.to_str().unwrap();
                ensure_running(
                    &exe,
                    &["--exact", "host_process_entry", socket_arg, "--nocapture"],
                    &socket,
                )
            })
        })
        .collect();
    let clients: Vec<Client> = callers
        .into_iter()
        .map(|caller| caller.join().unwrap().unwrap())
        .collect();

    let host_pid = clients[0].host().pid.unwrap();
    assert_ne!(host_pid, std::process::id());
    assert!(clients
        .iter()
        .all(|client| client.host().pid == Some(host_pid)));
    // Every caller may have started a process; all but the one that took
    // the socket must go away by themselves.
    wait_for("the hosts that lost the race to exit", || {
        (host_processes(&socket) == [host_pid]).then_some(())
    });

    // It is one host: a session started through one caller is there for
    // another.
    let (session, _) = clients[0].spawn(sh("read hold")).unwrap();
    assert_eq!(clients[7].list().unwrap()[0].id, session);

    // Asking again starts nothing.
    let again = ensure_running(Path::new("/nonexistent/host"), &[], &socket).unwrap();
    assert_eq!(again.host().pid, Some(host_pid));

    again.shutdown(true).unwrap();
    wait_for("the host to exit", || {
        host_processes(&socket).is_empty().then_some(())
    });
    assert!(!socket.exists());
}

#[test]
fn sessions_started_at_the_same_moment_each_get_their_own_terminal() {
    let host = TestHost::start(|_| {});
    let starters: Vec<_> = (0..12)
        .map(|n| {
            let client = host.client();
            std::thread::spawn(move || {
                let (session, _) = client.spawn(sh(&format!("echo session-{n}-end"))).unwrap();
                (n, session)
            })
        })
        .collect();
    let client = host.client();
    for starter in starters {
        let (n, session) = starter.join().unwrap();
        // Exactly its own line: nothing of another session, and nothing of
        // its own gone to another.
        assert_eq!(
            whole_session(&client, session).text(),
            format!("session-{n}-end\r\n")
        );
    }
}

#[test]
fn a_child_that_prints_and_exits_at_once_keeps_its_output() {
    // Many at a time on purpose. What is tested is a race between the
    // child's exit and the host's reader, and it only shows under load.
    let host = TestHost::start(|_| {});
    let workers: Vec<_> = (0..16)
        .map(|worker| {
            let client = host.client();
            std::thread::spawn(move || {
                for round in 0..20 {
                    let word = format!("worker-{worker}-round-{round}");
                    let (session, _) = client
                        .spawn(SpawnSpec {
                            argv: vec!["/bin/echo".into(), word.clone()],
                            cwd: "/".into(),
                            cols: 80,
                            rows: 24,
                            ..SpawnSpec::default()
                        })
                        .unwrap();
                    assert_eq!(
                        whole_session(&client, session).text(),
                        format!("{word}\r\n")
                    );
                    client.forget(session).unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}
