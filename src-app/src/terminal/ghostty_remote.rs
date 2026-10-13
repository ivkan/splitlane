//! A Ghostty pane whose PTY is in the session host.
//!
//! The pane keeps everything it has when it owns its PTY - the terminal,
//! the snapshots the window draws, the encoders for keys and the mouse,
//! selection, search - and loses the PTY. Output arrives as the events of
//! a stream instead of from a reader thread, and what the pane says goes to
//! the host over the pane's queue.
//!
//! # One queue, in the stream's order
//!
//! The terminal lives on one thread, the runtime, and can be used on no
//! other. The stream is taken by the pane's reader thread. So the reader
//! puts what the stream says on the runtime's queue - output in the queue's
//! own buffers, everything else as a [`HostEvent`] - and the runtime does it
//! in that order: a size the log recorded is applied between the bytes it
//! fell between, the end of the replay where the log ended, the child's end
//! after its last output.
//!
//! Nothing is put on the queue without room for it. The reader waits for a
//! free buffer, or for an event to be taken, before it accepts the next
//! thing from the stream, and a reader that does not take the stream leaves
//! it on the host's socket. A terminal that falls behind therefore holds up
//! nothing but its own connection.
//!
//! # What this runtime cannot do
//!
//! It opens no PTY, starts no process, waits for none and signals none: the
//! session is the host's. A pane ends its session only by letting go of its
//! handle, which is the terminal state's to do and which a window that is
//! going away does not do.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use splitlane_host::{ExitStatus, Facts, StreamEvent};

use super::super::host_session::{
    AttachTarget, CONNECTION_LOST_NOTICE, RemoteMsg, RemoteSender, ShellStart, StreamSink,
};
use super::super::stream_replay::{AttachMode, ReplayAction, SinkStep, StreamReplay, StreamStart};
use super::*;

/// How many events of a stream may wait on the runtime's queue before the
/// reader waits. A log can record any number of resizes in a row, and each
/// would otherwise be queued as fast as the socket delivers it.
const HOST_EVENT_CAPACITY: usize = 64;

/// What the stream of a session says besides output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostEvent {
    /// The grid had this size from here on, in the log.
    Resize { cols: u16, rows: u16 },
    /// The log ends here. What follows is happening now.
    GoLive,
    /// The session's child ended. Nothing follows.
    Exited(ExitStatus),
    /// The connection to the host is gone. Nothing follows.
    Lost,
}

impl RuntimeMailbox {
    /// Queue an event of the stream behind everything queued so far,
    /// waiting for room. False when the runtime is gone.
    fn send_host(&self, event: HostEvent) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if state.closed {
                return false;
            }
            if state.host_event_count < HOST_EVENT_CAPACITY {
                break;
            }
            state = self
                .host_event_ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.host_event_count += 1;
        state.queue.push_back(RuntimeMessage::Host(event));
        drop(state);
        self.ready.notify_one();
        true
    }

    /// Queue the pane's sender. Never refused and never waited for: it is
    /// sent once, by the thread that draws the window, and the input that
    /// thread queues next needs it to have arrived.
    fn send_host_attached(&self, sender: RemoteSender) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.queue.push_back(RuntimeMessage::HostAttached(sender));
        drop(state);
        self.ready.notify_one();
    }

    /// Queue a chunk of the stream's output, waiting for a free buffer.
    /// False when the runtime is gone.
    fn send_stream_output(&self, bytes: &[u8]) -> bool {
        for chunk in bytes.chunks(OUTPUT_CHUNK_BYTES) {
            let Some(mut buffer) = self.take_output_buffer() else {
                return false;
            };
            buffer.clear();
            buffer.extend_from_slice(chunk);
            if !self.send_output(buffer) {
                return false;
            }
        }
        true
    }
}

impl GhosttySession {
    /// Give the runtime the pane's queue to the host. From here on input
    /// and sizes reach the session.
    pub(in crate::terminal) fn attach_host(&self, sender: RemoteSender) {
        self.inner.mailbox.send_host_attached(sender);
    }

    /// Have the host told the window's size again, because what it last
    /// confirmed is not what it was sent.
    pub(in crate::terminal) fn resend_host_size(&self) {
        let _ = self
            .inner
            .mailbox
            .try_send_control(RuntimeMessage::ResendHostSize);
    }

    /// How many times the runtime dropped input the queue to the host had
    /// no room for.
    pub(in crate::terminal) fn refused_input(&self) -> u64 {
        self.inner.refused_input.load(Ordering::Relaxed)
    }
}

/// What a window tells the session host about a Ghostty pane: the colours
/// the pane's own terminal answers colour queries with, which are the
/// theme's as [`new_display_terminal`] gives them to it, and the size of a
/// cell once the window has laid one out.
///
/// No palette: the terminal leaves a query for an indexed colour
/// unanswered in a pane, and the host's does the same.
pub(in crate::terminal) fn window_facts(
    theme: &crate::theme::TerminalTheme,
    cell: Option<(u16, u16)>,
) -> Facts {
    let color = |color: gpui::Hsla| {
        let rgb = ghostty_rgb(color);
        Some(splitlane_host::Rgb(rgb.r, rgb.g, rgb.b))
    };
    Facts {
        foreground: color(theme.foreground),
        background: color(theme.ansi_background),
        cursor: color(theme.cursor),
        palette: Default::default(),
        cell_width: cell.map(|(width, _)| width),
        cell_height: cell.map(|(_, height)| height),
    }
}

/// A Ghostty pane that has no runtime yet, as what a session in the host
/// can be attached to. The runtime it is given then is this file's; taken
/// apart again, it can be given one that owns a PTY.
pub(in crate::terminal) struct GhosttyAttach {
    pub(in crate::terminal) session: GhosttySession,
    pub(in crate::terminal) pending: GhosttyRuntimePending,
    pub(in crate::terminal) max_scrollback: usize,
}

impl AttachTarget for GhosttyAttach {
    type Sink = GhosttySink;
    /// Everything the runtime reports reaches the terminal state the way
    /// it does for a pane that owns its PTY.
    type Channels = ();

    fn sink(&self, mode: AttachMode, start: StreamStart) -> (GhosttySink, ()) {
        let mailbox = self.pending.mailbox.clone();
        start_remote_runtime(
            self.session.inner.clone(),
            mailbox.clone(),
            self.max_scrollback,
            mode,
        );
        // The runtime is already replaying if it is to replay: what
        // opening the stream says is queued ahead of the stream itself.
        let replay = StreamReplay::new(mode, start, &mut |action| post(&mailbox, action));
        (GhosttySink { replay, mailbox }, ())
    }

    fn shell_start(params: &SpawnParams) -> anyhow::Result<ShellStart> {
        Ok(shell_start(&shell_command(params)))
    }
}

/// What the PTY library would start `command` with: its environment, and
/// the directory, which is the user's home when the one asked for is not
/// there.
fn shell_start(command: &CommandBuilder) -> ShellStart {
    let env: Vec<(String, String)> = command
        .iter_full_env_as_str()
        // What no process can be started with.
        .filter(|(key, value)| {
            !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0')
        })
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let cwd = command
        .get_cwd()
        .map(PathBuf::from)
        .filter(|cwd| cwd.is_dir())
        .unwrap_or_else(|| {
            let home = env
                .iter()
                .find(|(key, _)| key == "HOME")
                .map(|(_, home)| home.clone())
                .or_else(|| super::super::host_session::password_entry().map(|(_, home)| home));
            PathBuf::from(home.unwrap_or_else(|| "/".to_owned()))
        });
    ShellStart { env, cwd }
}

/// Do to the runtime what the stream's position says, by queueing it.
fn post(mailbox: &RuntimeMailbox, action: ReplayAction<'_>) {
    // A runtime that is gone takes nothing, and nothing is owed to it.
    let _ = match action {
        ReplayAction::ResizeGrid { cols, rows } => {
            mailbox.send_host(HostEvent::Resize { cols, rows })
        }
        // Old or new is not said with the bytes: it is which side of
        // `GoLive` they are queued on.
        ReplayAction::Feed { bytes, live: _ } => mailbox.send_stream_output(bytes),
        ReplayAction::GoLive => mailbox.send_host(HostEvent::GoLive),
        ReplayAction::Exited(status) => mailbox.send_host(HostEvent::Exited(status)),
    };
}

/// Puts the stream of a session in the host on a Ghostty runtime's queue.
/// Owned by the pane's reader thread.
pub(in crate::terminal) struct GhosttySink {
    replay: StreamReplay,
    mailbox: Arc<RuntimeMailbox>,
}

impl StreamSink for GhosttySink {
    fn handle(&mut self, event: StreamEvent) -> SinkStep {
        let mailbox = &self.mailbox;
        self.replay
            .handle(&event, &mut |action| post(mailbox, action))
    }

    /// The terminal shows a synchronized update as it reads it, so there
    /// is never anything held back to be let go of.
    fn sync_deadline(&self) -> Option<Instant> {
        None
    }

    fn quiet(&mut self) {}

    fn connection_lost(&mut self) {
        let mailbox = &self.mailbox;
        self.replay.abandon(&mut |action| post(mailbox, action));
        let _ = mailbox.send_host(HostEvent::Lost);
    }
}

/// Start the runtime of a pane in the host on a thread of its own, and
/// wait until it has a terminal. A runtime that cannot be started leaves
/// the queue closed and the pane saying why.
fn start_remote_runtime(
    inner: Arc<SessionInner>,
    mailbox: Arc<RuntimeMailbox>,
    max_scrollback: usize,
    mode: AttachMode,
) {
    let (started_tx, started_rx) = sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("splitlane-ghostty-remote".into())
        .spawn({
            let (inner, mailbox) = (inner.clone(), mailbox.clone());
            move || {
                let boundary_inner = inner.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_remote_runtime(inner, mailbox, max_scrollback, mode, started_tx);
                }));
                if result.is_err() {
                    fail(
                        &boundary_inner,
                        "Ghostty runtime worker terminated unexpectedly".to_owned(),
                    );
                }
            }
        });
    let started = match spawned {
        Ok(_) => started_rx.recv().unwrap_or_else(|error| {
            Err(anyhow::anyhow!(
                "Ghostty runtime exited before startup completed: {error}"
            ))
        }),
        Err(error) => {
            Err(anyhow::Error::new(error).context("could not start Ghostty runtime thread"))
        }
    };
    if let Err(error) = started {
        mailbox.close();
        fail(&inner, format!("{error:#}"));
    }
}

/// The runtime cannot go on. The pane says so and shows as ended; the
/// session is not touched, and ends when the pane is closed.
fn fail(inner: &SessionInner, error: String) {
    inner.shutdown_sent.store(true, Ordering::Release);
    stop_session_input(inner);
    let _ = inner
        .events_tx
        .unbounded_send(GhosttyUiEvent::RuntimeFailed(error));
    publish_child_exit_once(inner, -1, None);
}

/// How the host says a child ended, as a pane that owns its PTY reports
/// the end of its own: the code, or the code such a pane gives a child
/// that was killed and the signal's name. A host that could not say
/// reports what a runtime that lost its child does.
fn child_exit_from_host(status: ExitStatus) -> (i32, Option<String>) {
    match (status.signal, status.code) {
        (Some(signal), _) => {
            let killed = portable_pty::ExitStatus::with_signal(&signal_name(signal));
            (
                i32::try_from(killed.exit_code()).unwrap_or(-1),
                killed.signal().map(str::to_owned),
            )
        }
        (None, Some(code)) => (code, None),
        (None, None) => (-1, None),
    }
}

/// Whether the runtime's loop goes on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Continue,
    Stop,
}

/// The state of a runtime whose PTY is in the host.
struct RemoteRuntime {
    inner: Arc<SessionInner>,
    terminal: ghostty::DisplayTerminal,
    marks_scanner: Osc133Scanner,
    status: StatusTap,
    service_output_tail: ServiceOutputTail,
    last_recent_output_refresh: Option<Instant>,
    recent_output_pending: bool,
    mode: AttachMode,
    /// The output being read is the session's log, not its present.
    replaying: bool,
    held: HeldBack,
    /// The pane's queue to the host, once the pane has one.
    sender: Option<RemoteSender>,
    /// The size the window last asked for. `None` until it has laid the
    /// pane out: before that there is no size of the window's to give
    /// either the grid or the host.
    window_size: Option<TerminalWindowSize>,
    /// The size the host was last sent.
    told_host: Option<TerminalWindowSize>,
}

fn run_remote_runtime(
    inner: Arc<SessionInner>,
    mailbox: Arc<RuntimeMailbox>,
    max_scrollback: usize,
    mode: AttachMode,
    started: std::sync::mpsc::SyncSender<anyhow::Result<()>>,
) {
    let _mailbox_close = MailboxCloseGuard(mailbox.clone());
    let terminal = match new_display_terminal(&inner, max_scrollback) {
        Ok((terminal, _)) => terminal,
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    if started.send(Ok(())).is_err() {
        return;
    }
    let mut runtime = RemoteRuntime {
        status: StatusTap::new(inner.program_status.clone()),
        inner,
        terminal,
        marks_scanner: Osc133Scanner::default(),
        service_output_tail: ServiceOutputTail::default(),
        last_recent_output_refresh: None,
        recent_output_pending: false,
        mode,
        replaying: mode == AttachMode::Reattach,
        held: HeldBack::default(),
        sender: None,
        window_size: None,
        told_host: None,
    };
    loop {
        if runtime.inner.shutdown_sent.load(Ordering::Acquire) {
            break;
        }
        let step = match mailbox.recv_timeout(Duration::from_millis(10)) {
            Ok(message) => runtime.take(&mailbox, message),
            Err(MailboxRecvError::Timeout) => Step::Continue,
            Err(MailboxRecvError::Disconnected) => Step::Stop,
        };
        if step == Step::Stop {
            break;
        }
        runtime.idle();
    }
}

impl RemoteRuntime {
    fn take(&mut self, mailbox: &RuntimeMailbox, message: RuntimeMessage) -> Step {
        match message {
            RuntimeMessage::Output(bytes) => self.output(mailbox, bytes),
            RuntimeMessage::Host(event) => self.host_event(event),
            RuntimeMessage::HostAttached(sender) => {
                self.sender = Some(sender);
                // A size the window asked for before there was anybody to
                // tell. During a replay it waits for the replay's end.
                if !self.replaying {
                    self.tell_host_size(false);
                }
                Step::Continue
            }
            RuntimeMessage::ResendHostSize => {
                if !self.replaying {
                    self.tell_host_size(true);
                }
                Step::Continue
            }
            RuntimeMessage::Resize(command) => self.resize(command),
            RuntimeMessage::Shutdown => Step::Stop,
            message => {
                let Self {
                    inner,
                    terminal,
                    sender,
                    ..
                } = self;
                let message = handle_input_message(inner, terminal, message, &mut |bytes| {
                    forward_input(inner, sender.as_ref(), bytes);
                });
                // What is left is only ever sent to a runtime that owns a
                // PTY: the end of its reader's file.
                let _ = message.and_then(|message| handle_view_message(inner, terminal, message));
                Step::Continue
            }
        }
    }

    /// What the loop does between messages.
    fn idle(&mut self) {
        if !self.replaying
            && refresh_recent_output_lines(
                &self.inner,
                &self.service_output_tail,
                &mut self.last_recent_output_refresh,
                &mut self.recent_output_pending,
            )
        {
            queue_service_output_ready(&self.inner);
        }
        notify_command_capacity(&self.inner);
    }

    fn output(&mut self, mailbox: &RuntimeMailbox, bytes: Vec<u8>) -> Step {
        let held = self.replaying.then_some(&mut self.held);
        match process_output_batch(
            &self.inner,
            mailbox,
            &mut self.terminal,
            // The host has answered whatever these bytes ask.
            &mut Replies::AnsweredByHost,
            held,
            &mut self.marks_scanner,
            &mut self.status,
            &mut self.service_output_tail,
            &mut self.last_recent_output_refresh,
            &mut self.recent_output_pending,
            bytes,
        ) {
            Ok(()) => Step::Continue,
            Err(error) => {
                fail(&self.inner, error);
                Step::Stop
            }
        }
    }

    fn host_event(&mut self, event: HostEvent) -> Step {
        match event {
            HostEvent::Resize { cols, rows } => {
                // Only a replay is sent sizes, and only a replay may take
                // them: live, the grid is the window's.
                if self.replaying {
                    let (cell_width, cell_height) = self
                        .window_size
                        .map_or((0, 0), |size| (size.cell_width, size.cell_height));
                    let size = TerminalWindowSize::new(
                        usize::from(cols),
                        usize::from(rows),
                        cell_width,
                        cell_height,
                    );
                    if let Err(error) = resize_grid(&mut self.terminal, size, false) {
                        log::warn!(
                            target: "splitlane::terminal::ghostty",
                            "a size the session's log recorded, {cols}x{rows}, could not be given to the grid: {error}"
                        );
                    }
                }
                Step::Continue
            }
            HostEvent::GoLive => self.go_live(),
            HostEvent::Exited(status) => {
                self.show_what_is_there();
                let (code, signal) = child_exit_from_host(status);
                publish_child_exit_once(&self.inner, code, signal);
                Step::Stop
            }
            HostEvent::Lost => {
                // The pane says so and ends, as for a shell that died:
                // what became of the session cannot be known from here.
                let _ = self.terminal.feed(CONNECTION_LOST_NOTICE);
                // The notice asks nothing; whatever the terminal reports
                // while reading it is not the program's doing.
                drop(self.terminal.drain_events());
                self.show_what_is_there();
                publish_child_exit_once(&self.inner, -1, None);
                Step::Stop
            }
        }
    }

    /// Publish the grid and the last lines of output as they stand, for a
    /// runtime that is about to stop.
    fn show_what_is_there(&mut self) {
        if self.recent_output_pending {
            publish_recent_output_lines(
                &self.inner,
                &self.service_output_tail,
                &mut self.recent_output_pending,
            );
            queue_service_output_ready(&self.inner);
        }
        let _ = refresh_shared_state(&self.inner, &mut self.terminal);
    }

    /// The replay is over: the grid takes the window's size, what the log
    /// left behind is reported once, and the window is told that there is
    /// something to draw - which is not output, and is not counted as any.
    fn go_live(&mut self) -> Step {
        self.replaying = false;
        if let Some(size) = self.window_size
            && let Err(error) = resize_grid(&mut self.terminal, size, false)
        {
            log::warn!(
                target: "splitlane::terminal::ghostty",
                "the window's size, {}x{}, could not be given to the grid after a replay: {error}",
                size.cols,
                size.rows,
            );
        }
        if let Err(error) = update_shared_state(&self.inner, &mut self.terminal) {
            fail(&self.inner, error);
            return Step::Stop;
        }
        let held = std::mem::take(&mut self.held);
        if let Some(title) = held.title {
            queue_title(&self.inner, title);
        }
        if let Some(cwd) = held.working_directory {
            queue_working_directory(&self.inner, cwd);
        }
        if let Some(mark) = held.mark {
            record_command_marks(&self.inner, &[mark]);
        }
        // The lines the replay left on the screen, for the terminal state
        // to take note of without announcing them.
        publish_recent_output_lines(
            &self.inner,
            &self.service_output_tail,
            &mut self.recent_output_pending,
        );
        self.last_recent_output_refresh = Some(Instant::now());
        let _ = self
            .inner
            .events_tx
            .unbounded_send(GhosttyUiEvent::ReplayFinished);
        self.tell_host_size(false);
        Step::Continue
    }

    fn resize(&mut self, command: ResizeCommand) -> Step {
        let size = command.size;
        self.window_size = Some(size);
        if self.replaying {
            // The log has the grid until it ends, and the size is applied
            // then. The window's request is done with either way.
            complete_resize(&self.inner, command, true);
            return Step::Continue;
        }
        // A pane attached to a running session shows what the session
        // printed. The clear a new pane makes on its first layout, of
        // what its shell printed into a grid of the wrong size, would
        // wipe that.
        let clear = command.clear_initial && self.mode == AttachMode::Fresh;
        let resized = resize_grid(&mut self.terminal, size, clear).and_then(|()| {
            update_shared_state(&self.inner, &mut self.terminal)?;
            queue_wakeup(&self.inner);
            Ok(())
        });
        let succeeded = match resized {
            Ok(()) => true,
            Err(error) => {
                log::warn!(
                    target: "splitlane::terminal::ghostty",
                    "Ghostty resize to {}x{} failed: {error}",
                    size.cols,
                    size.rows,
                );
                false
            }
        };
        complete_resize(&self.inner, command, succeeded);
        if succeeded {
            self.tell_host_size(false);
        }
        Step::Continue
    }

    /// Have the host give the PTY the window's size. Once per size unless
    /// `again`; nothing before the window has a size or the pane a queue.
    fn tell_host_size(&mut self, again: bool) {
        let (Some(sender), Some(size)) = (&self.sender, self.window_size) else {
            return;
        };
        if !again && self.told_host == Some(size) {
            return;
        }
        self.told_host = Some(size);
        let cells = |count: usize| u16::try_from(count.max(1)).unwrap_or(u16::MAX);
        let (cols, rows) = (cells(size.cols), cells(size.rows));
        // The host is told the size of the whole text area, and works the
        // size of a cell out of it.
        sender.send(RemoteMsg::Resize {
            cols,
            rows,
            pixel_width: size.cell_width.saturating_mul(cols),
            pixel_height: size.cell_height.saturating_mul(rows),
        });
    }
}

/// Hand encoded input to the pane's queue. Input the queue has no room
/// for is dropped and counted, as it is for any pane in the host: a
/// program that reads nothing must not end the pane.
fn forward_input(inner: &SessionInner, sender: Option<&RemoteSender>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let accepted = sender.is_some_and(|sender| sender.input(Cow::Owned(bytes.to_vec())));
    if !accepted {
        inner.refused_input.fetch_add(1, Ordering::Relaxed);
    }
}
