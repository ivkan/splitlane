//! One PTY, the child running in it, and the log of what it printed.
//!
//! Three threads serve a session. The reader moves bytes from the PTY into
//! the log and never waits for anybody: whoever follows the session holds
//! only a position in the log, so a client that stops reading costs the
//! reader nothing and loses nothing until the ring wraps past it. The writer
//! feeds input to the PTY, so a child that does not read its input blocks
//! that one thread and not the connection that sent it. The waiter reaps
//! the child.
//!
//! The reader also shows every byte to the session's [`Engine`], the
//! emulator that answers the program's terminal queries, and hands the
//! answers to the writer. It never waits for that either: an answer the
//! writer has no room for is dropped.
//!
//! # Lock order
//!
//! `terminal`, then `engine`, then `shared`. A resize holds all three so
//! that the PTY, the engine and the log see it at the same point of the
//! output; the reader holds the last two for the same reason. No other
//! lock of a session is held together with any of these.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, MasterPty, PtySize, SlavePty};

use crate::descriptors;
use crate::engine::{self, Engine};
use crate::lock;
use crate::log::SessionLog;
use crate::program_status;
use crate::protocol::{AgentState, ExitStatus, Facts, SessionId, SessionInfo, SpawnSpec};

/// Argv[1] of the watcher process that kills a child's process group when
/// the process that started the watcher dies. The app binary implements it;
/// the host only starts it.
pub const PTY_GUARD_SUBCOMMAND: &str = "__splitlane-pty-guard";

/// How long output is still collected after the child was reaped. A child's
/// last words are already in the PTY when it exits, so this is normally one
/// short read; the bound is for a background process that inherited the PTY
/// and never stops writing.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);
/// Silence that ends the drain early.
const DRAIN_QUIET_MS: i32 = 20;

/// How far apart the two resizes of a redraw are. Long enough for a program
/// to have seen the first size before the second arrives, so that it gets
/// two changes and not one that cancels itself.
const REDRAW_GAP: Duration = Duration::from_millis(50);

const READ_BUF: usize = 64 * 1024;
/// Input chunks that may wait for the PTY. Bounded, so a sender is slowed
/// down instead of the host buffering what a stuck child will never read.
const INPUT_QUEUE: usize = 64;

/// The memory all logs together may use.
///
/// Each log has its own capacity; this is the bound on their sum, for a host
/// that holds many sessions. When the sum goes over, the largest log is cut
/// first, by the excess but by no more than half of itself, and the next
/// largest after it until the sum fits. The session that printed the most
/// pays, and a quiet session keeps its whole history.
pub(crate) struct LogBudget {
    cap: usize,
    total: AtomicUsize,
    members: Mutex<Vec<Weak<Session>>>,
}

impl LogBudget {
    pub(crate) fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            cap,
            total: AtomicUsize::new(0),
            members: Mutex::new(Vec::new()),
        })
    }

    fn enroll(&self, session: &Arc<Session>) {
        let mut members = lock(&self.members);
        members.retain(|member| member.strong_count() > 0);
        members.push(Arc::downgrade(session));
    }

    fn account(&self, before: usize, after: usize) {
        if after >= before {
            self.total.fetch_add(after - before, Ordering::Relaxed);
        } else {
            self.total.fetch_sub(before - after, Ordering::Relaxed);
        }
    }

    fn enforce(&self) {
        if self.total.load(Ordering::Relaxed) <= self.cap {
            return;
        }
        // One enforcer at a time; whoever arrives second leaves it to the
        // first and goes back to reading its PTY.
        let Ok(members) = self.members.try_lock() else {
            return;
        };
        let mut sessions: Vec<Arc<Session>> = members.iter().filter_map(Weak::upgrade).collect();
        // Each pass at least halves the largest log or sets it aside, so
        // this ends.
        for _ in 0..64 {
            let excess = self.total.load(Ordering::Relaxed).saturating_sub(self.cap);
            if excess == 0 {
                break;
            }
            let largest = sessions
                .iter()
                .enumerate()
                .map(|(at, session)| (lock(&session.shared).log.stored_bytes(), at))
                .max_by_key(|(stored, _)| *stored);
            let Some((stored, at)) = largest else {
                break;
            };
            if stored == 0 {
                break;
            }
            let target = stored.saturating_sub(excess).max(stored / 2);
            let session = sessions[at].clone();
            let mut shared = lock(&session.shared);
            let before = shared.log.stored_bytes();
            let dropped = shared.log.trim_to(target);
            self.account(before, shared.log.stored_bytes());
            drop(shared);
            if dropped == 0 {
                // A log is cut only where its output may be, and this one
                // offers no such place right now. The next largest pays
                // instead; this one is bounded by twice its own capacity.
                sessions.swap_remove(at);
            } else {
                // Debug, not info: a session printing at the limit gets
                // here on every read.
                log::debug!(
                    "session {}: log cut by {dropped} positions to keep all logs within {} bytes",
                    session.id,
                    self.cap
                );
            }
        }
    }
}

/// What a session needs from the host that owns it.
pub(crate) struct SessionSettings {
    pub(crate) log_capacity: usize,
    pub(crate) budget: Arc<LogBudget>,
    pub(crate) guard_exe: Option<PathBuf>,
    pub(crate) kill_grace: Duration,
}

/// State guarded by one lock and announced through one condition variable:
/// everything a follower of the session waits on.
pub(crate) struct Shared {
    pub(crate) log: SessionLog,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    /// Set once, after the last output is in the log.
    pub(crate) exit: Option<ExitStatus>,
    pub(crate) attached: u32,
}

/// A terminal size as the PTY is told it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Size {
    cols: u16,
    rows: u16,
    pixel_width: u16,
    pixel_height: u16,
}

impl Size {
    fn pty(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
        }
    }

    /// The nearest other size: what a redraw resizes to before it resizes
    /// back. One row less, because a program repaints everything when its
    /// height changes and may only re-wrap when its width does.
    fn nudged(self) -> Self {
        let (cols, rows) = match (self.cols, self.rows) {
            (cols, rows) if rows > 1 => (cols, rows - 1),
            (cols, rows) if cols > 1 => (cols - 1, rows),
            (cols, rows) => (cols, rows + 1),
        };
        Size { cols, rows, ..self }
    }
}

/// The PTY's master side and what the host tells it. One lock, held for
/// the whole of a resize, so that resizes reach the PTY, the engine and
/// the log in one order.
struct Terminal {
    /// Dropped when the session ends.
    master: Option<Box<dyn MasterPty + Send>>,
    /// The size a client asked for last. A redraw moves the PTY away from
    /// it for a moment and comes back to this, not to where it started.
    asked: Size,
    /// Size of a character cell in pixels, once a client has said.
    cell: Option<(u16, u16)>,
    /// A redraw is between its two resizes.
    redrawing: bool,
}

pub(crate) struct Session {
    pub(crate) id: SessionId,
    pub(crate) pid: u32,
    cwd: String,
    program: String,
    label: Option<String>,
    exported: BTreeMap<String, String>,
    created_at_ms: u64,
    pub(crate) shared: Mutex<Shared>,
    pub(crate) changed: Condvar,
    budget: Arc<LogBudget>,
    kill_grace: Duration,
    terminal: Mutex<Terminal>,
    /// Answers the program's terminal queries. `None` on a platform that
    /// has no engine, after the session ended, and after an engine failed.
    engine: Mutex<Option<Box<dyn Engine>>>,
    /// Finds the program's question whether its status reports are read.
    /// The answer does not depend on the emulator, so it is given on every
    /// platform, engine or none.
    status: Mutex<program_status::Scanner>,
    dropped_replies: AtomicU64,
    /// Held so the PTY keeps unread output; see where it is stored. With a
    /// slave open here the master never reports the end of output, which
    /// is why a session ends when its child is reaped and not when the
    /// PTY says so.
    slave: Mutex<Option<Box<dyn SlavePty + Send>>>,
    input: Mutex<Option<SyncSender<Vec<u8>>>>,
    /// The watcher exits when this pipe closes, so it is held for exactly as
    /// long as there is a child to watch.
    /// The pipe the session's watcher reads. Closing it tells the watcher
    /// that the session is over.
    guard: Mutex<Option<OwnedFd>>,
    /// Set by the waiter the moment the child is reaped, before the rest of
    /// its output is collected.
    reaped: AtomicBool,
    /// The newest `publish_state`; see [`AgentState`].
    agent_state: Mutex<Option<Published>>,
}

struct Published {
    state: serde_json::Value,
    at_ms: u64,
    /// The connection that published it, while that connection is open.
    publisher: Option<u64>,
}

impl Session {
    pub(crate) fn spawn(
        spec: SpawnSpec,
        settings: &SessionSettings,
    ) -> Result<Arc<Session>, String> {
        let Some(program) = spec.argv.first().cloned() else {
            return Err("argv is empty".into());
        };
        if spec.cols == 0 || spec.rows == 0 {
            return Err(format!(
                "terminal size {}x{} is empty",
                spec.cols, spec.rows
            ));
        }
        // Checked here because the alternative is the child starting
        // somewhere else without anybody being told.
        if !Path::new(&spec.cwd).is_dir() {
            return Err(format!(
                "working directory {:?} is not a directory",
                spec.cwd
            ));
        }

        let cell = spec
            .facts
            .as_ref()
            .and_then(|facts| facts.cell_width.zip(facts.cell_height));
        let size = Size {
            cols: spec.cols,
            rows: spec.rows,
            pixel_width: cell.map_or(0, |(width, _)| width.saturating_mul(spec.cols)),
            pixel_height: cell.map_or(0, |(_, height)| height.saturating_mul(spec.rows)),
        };
        let mut engine = engine::for_this_platform(spec.cols, spec.rows);
        if let (Some(engine), Some(facts)) = (engine.as_mut(), spec.facts.as_ref()) {
            engine.set_facts(facts);
        }

        let opened = {
            // One session is opened at a time. Opening PTYs from several
            // threads at once is not safe on macOS, where `openpty` keeps
            // process-wide state: concurrent calls were seen to fail
            // outright, and to leave a session whose log never received
            // what its child printed. The lock also covers the moments in
            // which a descriptor exists but is not yet marked
            // close-on-exec, during which a child forked for another
            // session would inherit it and keep that PTY open.
            let _opening = lock(&OPENING);
            open_session(&spec, size, &program, settings.guard_exe.as_deref())?
        };
        let Opened {
            master,
            slave,
            read_fd,
            write_fd,
            wake_rx,
            wake_tx,
            pid,
            guard,
        } = opened;
        let (input_tx, input_rx) = sync_channel::<Vec<u8>>(INPUT_QUEUE);

        let session = Arc::new(Session {
            id: SessionId::new_random(),
            pid,
            cwd: spec.cwd,
            program,
            label: spec.label,
            exported: spec.exported,
            created_at_ms: now_ms(),
            shared: Mutex::new(Shared {
                log: SessionLog::new(settings.log_capacity, spec.cols, spec.rows),
                cols: spec.cols,
                rows: spec.rows,
                exit: None,
                attached: 0,
            }),
            changed: Condvar::new(),
            budget: settings.budget.clone(),
            kill_grace: settings.kill_grace,
            terminal: Mutex::new(Terminal {
                master: Some(master),
                asked: size,
                cell,
                redrawing: false,
            }),
            engine: Mutex::new(engine),
            status: Mutex::new(program_status::Scanner::new()),
            dropped_replies: AtomicU64::new(0),
            slave: Mutex::new(Some(slave)),
            input: Mutex::new(Some(input_tx)),
            guard: Mutex::new(guard),
            reaped: AtomicBool::new(false),
            agent_state: Mutex::new(None),
        });
        settings.budget.enroll(&session);

        let waiter = {
            let session = session.clone();
            thread("waiter", move || {
                let status = wait_for(pid);
                session.reaped.store(true, Ordering::SeqCst);
                // Wakes the reader out of its wait for output.
                let _ = (&File::from(wake_tx)).write_all(&[1]);
                status
            })
        };
        let waiter = match waiter {
            Ok(waiter) => waiter,
            Err(e) => {
                session.abandon();
                return Err(e);
            }
        };
        let reader = {
            let session = session.clone();
            thread("reader", move || {
                session.pump_output(read_fd, wake_rx);
                let status = waiter.join().unwrap_or_default();
                session.finish(status);
            })
        };
        let writer = thread("writer", move || {
            write_input(File::from(write_fd), input_rx)
        });
        if let Err(e) = reader.and(writer) {
            session.abandon();
            return Err(e);
        }
        log::info!(
            "session {}: started {:?} as pid {pid}, {}x{}",
            session.id,
            session.program,
            spec.cols,
            spec.rows
        );
        Ok(session)
    }

    /// Undo a spawn that could not be completed.
    fn abandon(&self) {
        signal_group(self.pid, libc::SIGKILL);
        lock(&self.guard).take();
        lock(&self.slave).take();
        lock(&self.terminal).master.take();
        lock(&self.input).take();
        lock(&self.engine).take();
    }

    /// Body of the reader thread: move output into the log until the child
    /// is gone and the PTY went quiet. The PTY reporting its own end stops
    /// it too, though while the host holds the slave that does not happen.
    fn pump_output(&self, pty: OwnedFd, wake: OwnedFd) {
        let mut buf = vec![0u8; READ_BUF];
        let mut replies = Vec::new();
        let mut reaped_at: Option<Instant> = None;
        loop {
            if reaped_at.is_none() && self.reaped.load(Ordering::SeqCst) {
                reaped_at = Some(Instant::now());
            }
            let timeout_ms = if reaped_at.is_some() {
                DRAIN_QUIET_MS
            } else {
                self.held_output_wait_ms()
            };
            let mut fds = [
                libc::pollfd {
                    fd: pty.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    // Watched only until the waiter has spoken; after that
                    // the pipe stays readable and would turn the wait into
                    // a spin.
                    fd: if reaped_at.is_some() {
                        -1
                    } else {
                        wake.as_raw_fd()
                    },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: `fds` is a valid array of two entries for the call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
            if ready < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if ready == 0 {
                if reaped_at.is_some() {
                    // The child is gone and the PTY has nothing more.
                    break;
                }
                // The other timeout: the program began a synchronized
                // update and went quiet without ending it.
                self.release_held_output(&mut replies);
                continue;
            }
            if fds[0].revents & libc::POLLIN != 0 {
                // SAFETY: `buf` is valid for `buf.len()` bytes.
                let n = unsafe { libc::read(pty.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n > 0 {
                    self.append_output(&buf[..n as usize], &mut replies);
                } else if n == 0 {
                    break;
                } else {
                    let kind = io::Error::last_os_error().kind();
                    if !matches!(kind, io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) {
                        // EIO is how Linux says every slave end is closed.
                        break;
                    }
                }
            } else if fds[0].revents != 0 {
                // Hang-up or error with nothing left to read.
                break;
            }
            if reaped_at.is_some_and(|at| at.elapsed() > DRAIN_AFTER_EXIT) {
                break;
            }
        }
    }

    fn append_output(&self, bytes: &[u8], replies: &mut Vec<u8>) {
        // The engine first and under the same locks as the log: a resize
        // then falls between the same two bytes for both.
        let mut engine = lock(&self.engine);
        // Ahead of anything the engine answers. A program that asks whether
        // its status is read asks what the terminal is right after, because
        // every terminal answers that: whichever answer it reads first
        // settles it.
        lock(&self.status).feed(bytes, &mut |event| {
            if let program_status::Event::Query { bel } = event {
                replies.extend_from_slice(program_status::reply(bel));
            }
        });
        self.run_engine(&mut engine, |engine| engine.advance(bytes, replies));
        let mut shared = lock(&self.shared);
        let before = shared.log.stored_bytes();
        shared.log.append_output(bytes);
        self.budget.account(before, shared.log.stored_bytes());
        drop(shared);
        drop(engine);
        self.changed.notify_all();
        self.answer(replies);
        self.budget.enforce();
    }

    /// Run `step` on the engine if there is one. An engine that panics is
    /// dropped and the session goes on without answers: the alternative is
    /// a reader thread that is gone and a session that never ends.
    fn run_engine(&self, engine: &mut Option<Box<dyn Engine>>, step: impl FnOnce(&mut dyn Engine)) {
        let Some(running) = engine.as_mut() else {
            return;
        };
        if catch_unwind(AssertUnwindSafe(|| step(running.as_mut()))).is_err() {
            log::error!(
                "session {}: the terminal engine failed; its queries go unanswered from here",
                self.id
            );
            *engine = None;
        }
    }

    /// Hand the engine's answers to the writer and empty `replies`.
    ///
    /// Never waits. The queue is full only when the program has stopped
    /// reading its input, and a program in that state is not waiting for
    /// an answer; blocking here would stop its output from being logged.
    fn answer(&self, replies: &mut Vec<u8>) {
        if replies.is_empty() {
            return;
        }
        let bytes = std::mem::take(replies);
        let len = bytes.len();
        if let Some(Err(TrySendError::Full(_))) = lock(&self.input)
            .as_ref()
            .map(|input| input.try_send(bytes))
        {
            self.dropped_replies.fetch_add(1, Ordering::Relaxed);
            log::debug!(
                "session {}: dropped {len} bytes of answers to terminal queries; the program is not reading its input",
                self.id
            );
        }
    }

    /// How long the reader may wait for output, in milliseconds as `poll`
    /// takes them: for ever, unless the engine is holding output back for
    /// a synchronized update.
    fn held_output_wait_ms(&self) -> i32 {
        let deadline = lock(&self.engine)
            .as_ref()
            .and_then(|engine| engine.sync_deadline());
        match deadline {
            // Rounded up, so that the wait is over when the deadline is.
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                i32::try_from(left.as_millis()).map_or(i32::MAX, |ms| ms.saturating_add(1))
            }
            None => -1,
        }
    }

    fn release_held_output(&self, replies: &mut Vec<u8>) {
        let mut engine = lock(&self.engine);
        self.run_engine(&mut engine, |engine| engine.end_sync(replies));
        drop(engine);
        self.answer(replies);
    }

    /// Record the end of the session. Runs after the last output is in the
    /// log, so a follower that sees the exit has seen everything before it.
    fn finish(&self, status: ExitStatus) {
        lock(&self.guard).take();
        lock(&self.slave).take();
        lock(&self.terminal).master.take();
        lock(&self.input).take();
        lock(&self.engine).take();
        lock(&self.shared).exit = Some(status);
        self.changed.notify_all();
        match (status.code, status.signal) {
            (_, Some(signal)) => log::info!("session {}: ended by signal {signal}", self.id),
            (code, None) => log::info!(
                "session {}: exited with code {}",
                self.id,
                code.unwrap_or(-1)
            ),
        }
    }

    pub(crate) fn exit_status(&self) -> Option<ExitStatus> {
        lock(&self.shared).exit
    }

    /// Queue input for the PTY. Input for a session that ended is dropped:
    /// there is nobody to read it and nobody to tell.
    pub(crate) fn input(&self, bytes: &[u8]) {
        let sender = lock(&self.input).clone();
        if let Some(sender) = sender {
            // Blocks when the queue is full. That holds up the connection
            // that is sending and nothing else.
            let _ = sender.send(bytes.to_vec());
        }
    }

    /// Resize the terminal to what a client asked for. `pixels` is the size
    /// of the text area in pixels, when the client knows it.
    pub(crate) fn resize(
        &self,
        cols: u16,
        rows: u16,
        pixels: Option<(u16, u16)>,
    ) -> Result<(), String> {
        if cols == 0 || rows == 0 {
            return Err(format!("terminal size {cols}x{rows} is empty"));
        }
        let mut terminal = lock(&self.terminal);
        // An area smaller than its own grid has no cell size to speak of;
        // answering a program with zero would be worse than not answering.
        let pixels = pixels.filter(|(width, height)| *width >= cols && *height >= rows);
        if let Some((width, height)) = pixels {
            terminal.cell = Some((width / cols, height / rows));
        }
        // Without pixels of its own a resize keeps the cell it had, so the
        // PTY does not go back to reporting none.
        let (pixel_width, pixel_height) = pixels.unwrap_or_else(|| {
            terminal.cell.map_or((0, 0), |(width, height)| {
                (width.saturating_mul(cols), height.saturating_mul(rows))
            })
        });
        let size = Size {
            cols,
            rows,
            pixel_width,
            pixel_height,
        };
        self.apply_size(&terminal, size)?;
        terminal.asked = size;
        Ok(())
    }

    /// Tell the PTY, the engine and the log about a size. The caller holds
    /// `terminal`, which is what puts two resizes into all three in the
    /// same order.
    fn apply_size(&self, terminal: &Terminal, size: Size) -> Result<(), String> {
        let Some(pty) = terminal.master.as_ref() else {
            return Err("the session has ended".into());
        };
        pty.resize(size.pty())
            .map_err(|e| format!("cannot resize the PTY: {e:#}"))?;
        let mut engine = lock(&self.engine);
        self.run_engine(&mut engine, |engine| {
            engine.resize(size.cols, size.rows, terminal.cell)
        });
        let mut shared = lock(&self.shared);
        let before = shared.log.stored_bytes();
        shared.log.append_resize(size.cols, size.rows);
        self.budget.account(before, shared.log.stored_bytes());
        shared.cols = size.cols;
        shared.rows = size.rows;
        drop(shared);
        drop(engine);
        self.changed.notify_all();
        Ok(())
    }

    /// Make the program repaint its screen.
    ///
    /// A program repaints when its terminal changes size and at no other
    /// time it can be asked to, so the terminal is resized by one row and,
    /// a moment later, back. Both are ordinary resizes: the program gets
    /// the signal twice and both are in the log.
    ///
    /// The second step goes to whatever size a client asked for last, so a
    /// real resize that arrives between the two is not undone. A redraw
    /// asked for while one is under way is that one.
    pub(crate) fn redraw(self: &Arc<Self>) -> Result<(), String> {
        let mut terminal = lock(&self.terminal);
        if terminal.redrawing && terminal.master.is_some() {
            return Ok(());
        }
        self.apply_size(&terminal, terminal.asked.nudged())?;
        terminal.redrawing = true;
        drop(terminal);
        let session = self.clone();
        let spawned = thread("redraw", move || {
            std::thread::sleep(REDRAW_GAP);
            session.finish_redraw();
        });
        if spawned.is_err() {
            // Back at once rather than never.
            self.finish_redraw();
        }
        Ok(())
    }

    fn finish_redraw(&self) {
        let mut terminal = lock(&self.terminal);
        terminal.redrawing = false;
        // The session may have ended in between, and then there is nothing
        // to put back.
        let _ = self.apply_size(&terminal, terminal.asked);
    }

    /// Replace what the host knows about the window showing this session.
    ///
    /// A new cell size changes the answers from here on. The PTY's own
    /// pixel size follows at the next resize, as it does in a terminal
    /// whose font changed without its grid changing: telling the PTY now
    /// would signal the program about a resize that did not happen.
    pub(crate) fn set_facts(&self, facts: &Facts) {
        let mut terminal = lock(&self.terminal);
        if let Some(cell) = facts.cell_width.zip(facts.cell_height) {
            terminal.cell = Some(cell);
        }
        let mut engine = lock(&self.engine);
        self.run_engine(&mut engine, |engine| engine.set_facts(facts));
    }

    /// Ask the session's process group to end, and make it after a grace.
    ///
    /// The child is the leader of its own session, so its pid names the
    /// group. The request goes out only while that is still true of the
    /// pid, which also covers a child that is already gone. The follow-up
    /// asks whether the group still has members rather than whether its
    /// leader lives: a process that ignores the request usually outlives the
    /// shell that started it, and is exactly what the SIGKILL is for.
    pub(crate) fn kill(&self) {
        if self.exit_status().is_some() || !self.terminate() {
            return;
        }
        let pid = self.pid;
        let grace = self.kill_grace;
        let spawned = thread("killer", move || {
            std::thread::sleep(grace);
            signal_group(pid, libc::SIGKILL);
        });
        if spawned.is_err() {
            signal_group(pid, libc::SIGKILL);
        }
    }

    /// SIGHUP and then SIGTERM to the group. True when they were
    /// delivered.
    ///
    /// The hangup is what a shell gets when its terminal is closed under
    /// it, and what makes an interactive one save its history on the way
    /// out; several ignore SIGTERM altogether and would otherwise be left
    /// for the SIGKILL. SIGTERM follows for the programs that only handle
    /// that one.
    ///
    /// Not sent once the child was reaped: from then on its pid may belong
    /// to somebody else, and what is left of the group is dealt with by the
    /// SIGKILL of whoever called this.
    pub(crate) fn terminate(&self) -> bool {
        !self.reaped.load(Ordering::SeqCst)
            && is_group_leader(self.pid)
            && signal_group(self.pid, libc::SIGHUP)
            && signal_group(self.pid, libc::SIGTERM)
    }

    /// SIGKILL to whatever is left of the group.
    pub(crate) fn kill_now(&self) {
        signal_group(self.pid, libc::SIGKILL);
    }

    pub(crate) fn group_alive(&self) -> bool {
        signal_group(self.pid, 0)
    }

    pub(crate) fn info(&self) -> SessionInfo {
        let shared = lock(&self.shared);
        SessionInfo {
            id: self.id,
            pid: self.pid,
            cols: shared.cols,
            rows: shared.rows,
            cwd: self.cwd.clone(),
            program: self.program.clone(),
            label: self.label.clone(),
            created_at_ms: self.created_at_ms,
            exited: shared.exit,
            log_head_seq: shared.log.head_seq(),
            log_tail_seq: shared.log.tail_seq(),
            log_bytes: shared.log.stored_bytes() as u64,
            attached: shared.attached,
            exported: self.exported.clone(),
            dropped_replies: self.dropped_replies.load(Ordering::Relaxed),
            agent_state: lock(&self.agent_state)
                .as_ref()
                .map(|published| AgentState {
                    state: published.state.clone(),
                    published_at_ms: published.at_ms,
                    live: published.publisher.is_some(),
                }),
        }
    }

    /// The session's history and screen as text; see
    /// [`crate::transcript`]. The log is copied under the lock and replayed
    /// after it is let go, so a read holds up the session's output for no
    /// longer than a copy.
    pub(crate) fn read_text(
        &self,
    ) -> Result<crate::protocol::ReadTextResult, crate::transcript::Unavailable> {
        let (cols, rows, preamble, items, gap) = {
            let shared = lock(&self.shared);
            let head = shared.log.head_seq();
            let (cols, rows) = shared.log.size_at(head);
            let mut items = Vec::new();
            let mut seq = head;
            loop {
                let batch = shared.log.read_from(seq, usize::MAX);
                if batch.items.is_empty() {
                    break;
                }
                seq = batch.next_seq;
                items.extend(batch.items);
            }
            (cols, rows, shared.log.head_preamble(), items, head > 0)
        };
        let text = crate::transcript::transcribe(cols, rows, &preamble, &items)?;
        Ok(crate::protocol::ReadTextResult { text, gap })
    }

    /// Keep `state` as what connection `publisher` says about the session.
    /// `null` takes back whatever was said.
    pub(crate) fn publish_state(&self, state: serde_json::Value, publisher: u64) {
        *lock(&self.agent_state) = (!state.is_null()).then(|| Published {
            state,
            at_ms: now_ms(),
            publisher: Some(publisher),
        });
    }

    /// Connection `publisher` closed. A state it published stays, marked as
    /// no longer kept current; one published since by another connection
    /// is not touched.
    pub(crate) fn publisher_gone(&self, publisher: u64) {
        if let Some(published) = lock(&self.agent_state).as_mut() {
            if published.publisher == Some(publisher) {
                published.publisher = None;
            }
        }
    }

    /// Give the log's memory back when the session is forgotten.
    pub(crate) fn release_log(&self) {
        let mut shared = lock(&self.shared);
        let before = shared.log.stored_bytes();
        shared.log.clear();
        self.budget.account(before, shared.log.stored_bytes());
    }
}

/// Serialises [`open_session`]; see where it is taken.
static OPENING: Mutex<()> = Mutex::new(());

/// Everything of a session that involves creating a descriptor or a
/// process.
struct Opened {
    master: Box<dyn MasterPty + Send>,
    slave: Box<dyn SlavePty + Send>,
    read_fd: OwnedFd,
    write_fd: OwnedFd,
    wake_rx: OwnedFd,
    wake_tx: OwnedFd,
    pid: u32,
    guard: Option<OwnedFd>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

fn open_session(
    spec: &SpawnSpec,
    size: Size,
    program: &str,
    guard_exe: Option<&Path>,
) -> Result<Opened, String> {
    let pair = native_pty_system()
        .openpty(size.pty())
        .map_err(|e| format!("cannot open a PTY: {e:#}"))?;
    let master_fd = pair
        .master
        .as_raw_fd()
        .ok_or("the PTY has no file descriptor")?;
    // SAFETY: `master_fd` belongs to `pair.master`, which is alive for the
    // whole borrow.
    let master_ref = unsafe { BorrowedFd::borrow_raw(master_fd) };
    expect_utf8_input(master_ref);
    let read_fd = clone_fd(master_ref, "reader")?;
    let write_fd = clone_fd(master_ref, "writer")?;
    let (wake_rx, wake_tx) = wake_pipe().map_err(|e| format!("cannot open a pipe: {e}"))?;

    let child = spawn_child(spec, program, pair.master.as_ref())?;
    let pid = child.id();
    let guard = guard_exe.and_then(|exe| spawn_guard(exe, pid).or_else(|| fork_watcher(pid)));
    Ok(Opened {
        // Kept, and not closed now that the child has its own copies. On
        // macOS, output nobody has read yet is thrown away when the last
        // slave descriptor closes, and a child that prints and exits at
        // once closes it before the reader has been scheduled: under load
        // about one such exit in four hundred lost everything it printed.
        // While the host holds a copy there is no last close until the
        // output has been collected.
        slave: pair.slave,
        master: pair.master,
        read_fd,
        write_fd,
        wake_rx,
        wake_tx,
        pid,
        guard,
    })
}

/// Tell the terminal that what is typed into it is UTF-8.
///
/// A new PTY does not assume so. Without the flag the line discipline
/// erases one byte for a backspace, which leaves the first bytes of a
/// multi-byte character on the line a program is about to read. The
/// window's own PTYs have it set by the emulator that opens them; a failure
/// here is not worth refusing a session over.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn expect_utf8_input(master: BorrowedFd<'_>) {
    // SAFETY: `termios` is plain integers, so all-zero is a valid value,
    // and both calls get a descriptor that is open for their duration and
    // a valid pointer to it.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(master.as_raw_fd(), &mut termios) == 0 {
            termios.c_iflag |= libc::IUTF8;
            libc::tcsetattr(master.as_raw_fd(), libc::TCSANOW, &termios);
        }
    }
}

/// The flag is not everywhere, and where it is missing the terminal is left
/// as it was opened.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn expect_utf8_input(_master: BorrowedFd<'_>) {}

fn thread<T: Send + 'static>(
    role: &str,
    body: impl FnOnce() -> T + Send + 'static,
) -> Result<std::thread::JoinHandle<T>, String> {
    std::thread::Builder::new()
        .name(format!("host-{role}"))
        .spawn(body)
        .map_err(|e| format!("cannot start the {role} thread: {e}"))
}

fn clone_fd(fd: BorrowedFd<'_>, role: &str) -> Result<OwnedFd, String> {
    fd.try_clone_to_owned()
        .map_err(|e| format!("cannot duplicate the PTY for the {role}: {e}"))
}

/// A pipe whose both ends close on exec, so no session's child inherits the
/// ends that belong to another session.
fn wake_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe` writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned by nobody
    // else.
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for end in [&ends.0, &ends.1] {
        // SAFETY: plain fcntl on a descriptor we own.
        if unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(ends)
}

/// Start the child on the slave side of the PTY.
///
/// The PTY comes from `portable-pty`, but the child is started here and not
/// through its `spawn_command`: that call always adds a `SHELL` variable of
/// its own and falls back to the home directory when the working directory
/// is missing, and the contract promises the child exactly the environment
/// and directory it was given.
fn spawn_child(
    spec: &SpawnSpec,
    program: &str,
    master: &(dyn MasterPty + Send),
) -> Result<std::process::Child, String> {
    let tty = master
        .tty_name()
        .ok_or("the PTY has no device name to open")?;
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&tty)
        .map_err(|e| format!("cannot open {}: {e}", tty.display()))?;
    let stdio = || {
        slave
            .try_clone()
            .map(Stdio::from)
            .map_err(|e| format!("cannot duplicate {}: {e}", tty.display()))
    };

    let mut command = Command::new(program);
    command
        .args(&spec.argv[1..])
        .env_clear()
        .envs(spec.env.iter().map(|(key, value)| (key, value)))
        .current_dir(&spec.cwd)
        .stdin(stdio()?)
        .stdout(stdio()?)
        .stderr(stdio()?);
    let descriptor_limit = descriptors::limit();
    // SAFETY: the closure runs between fork and exec and calls only
    // async-signal-safe functions.
    unsafe {
        command.pre_exec(move || {
            // Nothing of the host's may reach the child: not a client's
            // connection, not another session's terminal.
            descriptors::seal_inherited(descriptor_limit);
            descriptors::restore_limit_for_child();
            // The host handles SIGTERM and SIGINT itself and a handler does
            // not survive exec, but an ignored signal and a blocked one do.
            for signal in [
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
                libc::SIGPIPE,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            let mut none: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut none);
            libc::sigprocmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
            // A session of its own makes the child a process group leader,
            // which is what lets the whole group be signalled by its pid,
            // and lets the PTY become its controlling terminal.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // Without a controlling terminal a resize sends no SIGWINCH
            // and ^C reaches nobody. The request constant's type differs
            // between platforms, hence the inferred cast.
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|e| format!("cannot start {program:?}: {e}"))
}

/// Start the watcher that kills the child's process group if this process
/// dies without doing it. Same arguments and the same pipe convention as
/// the app uses for the PTYs it owns itself: the watcher exits when its
/// standard input closes.
fn spawn_guard(exe: &Path, child_pgid: u32) -> Option<OwnedFd> {
    let mut command = Command::new(exe);
    command
        .arg(PTY_GUARD_SUBCOMMAND)
        .arg(std::process::id().to_string())
        .arg(child_pgid.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // A group of its own, so a signal aimed at the host's group or the
        // child's does not take the watcher with it.
        .process_group(0);
    let descriptor_limit = descriptors::limit();
    // SAFETY: only `fcntl` is called between fork and exec.
    unsafe {
        command.pre_exec(move || {
            descriptors::seal_inherited(descriptor_limit);
            Ok(())
        });
    }
    match command.spawn() {
        Ok(mut guard) => {
            let Some(stdin) = guard.stdin.take() else {
                log::warn!("watcher for process group {child_pgid} has no control pipe");
                let _ = guard.kill();
                let _ = guard.wait();
                return None;
            };
            // Reaped on a thread of its own, or every ended session would
            // leave a zombie behind.
            if let Err(e) = thread("guard-reaper", move || {
                let _ = guard.wait();
            }) {
                log::warn!("{e}");
            }
            Some(OwnedFd::from(stdin))
        }
        Err(e) => {
            log::warn!(
                "cannot start the watcher {} for process group {child_pgid}: {e}; \
                 watching from a copy of this process instead",
                exe.display()
            );
            None
        }
    }
}

/// How long a forked watcher whose pipe closed goes on asking whether the
/// host is gone, and how often.
const WATCHER_RECHECKS: u32 = 40;
const WATCHER_RECHECK_NANOS: libc::c_long = 50_000_000;

/// Watch over the child's process group from a forked copy of this process,
/// for when the watcher binary cannot be started.
///
/// A host outlives the release that started it. An update replaces the app
/// on disk, and from then on the binary this host was started from may be
/// somewhere else or gone, while the host goes on starting sessions. A
/// copy of the host needs no file: it is forked, keeps nothing but one end
/// of a pipe, and does what the watcher binary does.
fn fork_watcher(child_pgid: u32) -> Option<OwnedFd> {
    let pgid = libc::pid_t::try_from(child_pgid)
        .ok()
        .filter(|id| *id > 1)?;
    let (pipe_rx, pipe_tx) = match wake_pipe() {
        Ok(ends) => ends,
        Err(e) => {
            log::warn!("no watcher for process group {child_pgid}: cannot open a pipe: {e}");
            return None;
        }
    };
    // SAFETY: getpid has no preconditions.
    let host = unsafe { libc::getpid() };
    let descriptor_limit = descriptors::limit();
    // SAFETY: the child calls only async-signal-safe functions and never
    // returns; see `watch_from_fork`.
    match unsafe { libc::fork() } {
        -1 => {
            log::warn!(
                "no watcher for process group {child_pgid}: {}",
                io::Error::last_os_error()
            );
            None
        }
        0 => watch_from_fork(pipe_rx.as_raw_fd(), descriptor_limit, host, pgid),
        watcher => {
            if let Err(e) = thread("guard-reaper", move || {
                let mut status: libc::c_int = 0;
                // SAFETY: `status` is a valid out-pointer and `watcher` is
                // this process's own child.
                while unsafe { libc::waitpid(watcher, &mut status, 0) } == -1
                    && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
                {
                }
            }) {
                log::warn!("{e}");
            }
            Some(pipe_tx)
        }
    }
}

/// Body of a forked watcher. It runs in a copy of a process that had other
/// threads, any of which may have held a lock at the fork, so it may call
/// nothing that takes one: no allocation, no logging, only system calls.
fn watch_from_fork(
    pipe: RawFd,
    descriptor_limit: RawFd,
    host: libc::pid_t,
    pgid: libc::pid_t,
) -> ! {
    // SAFETY: every call below is async-signal-safe, and the pointers
    // passed are to locals that live for the call.
    unsafe {
        // A group of its own, so a signal aimed at the host's group does
        // not take the watcher with it.
        libc::setpgid(0, 0);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        // Everything the host had open, closed: its listener, its clients,
        // every session's terminal, and the lock file - a copy of that one
        // left open would keep the next host from starting.
        for fd in 0..descriptor_limit {
            if fd != pipe {
                libc::close(fd);
            }
        }
        // The host never writes; the read returns when the pipe closes,
        // because the session ended or because the host did.
        let mut byte = 0u8;
        loop {
            let read = libc::read(pipe, (&raw mut byte).cast(), 1);
            let interrupted =
                read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted;
            if read <= 0 && !interrupted {
                break;
            }
        }
        // A dying process closes its files before its children are handed
        // to another parent, so "the pipe closed and the host is still my
        // parent" is asked more than once before it is believed.
        let pause = libc::timespec {
            tv_sec: 0,
            tv_nsec: WATCHER_RECHECK_NANOS,
        };
        for _ in 0..WATCHER_RECHECKS {
            let gone = libc::kill(-pgid, 0) != 0
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if gone {
                break;
            }
            if libc::getppid() != host {
                libc::kill(-pgid, libc::SIGHUP);
                libc::kill(-pgid, libc::SIGTERM);
                libc::nanosleep(&pause, std::ptr::null_mut());
                libc::nanosleep(&pause, std::ptr::null_mut());
                libc::kill(-pgid, libc::SIGKILL);
                break;
            }
            libc::nanosleep(&pause, std::ptr::null_mut());
        }
        libc::_exit(0)
    }
}

/// Body of the writer thread.
fn write_input(mut pty: File, input: Receiver<Vec<u8>>) {
    while let Ok(bytes) = input.recv() {
        if pty.write_all(&bytes).is_err() {
            // The slave side is closed. Keep emptying the queue so that a
            // sender blocked on it is released.
            while input.recv().is_ok() {}
            return;
        }
    }
}

/// Reap `pid` and report how it ended.
fn wait_for(pid: u32) -> ExitStatus {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: `status` is a valid out-pointer; `pid` is our own child.
        let rc = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
        if rc == -1 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // Somebody else reaped it. That it ended is all we know.
            return ExitStatus::default();
        }
        break;
    }
    if libc::WIFSIGNALED(status) {
        ExitStatus {
            code: None,
            signal: Some(libc::WTERMSIG(status)),
        }
    } else {
        ExitStatus {
            code: Some(libc::WEXITSTATUS(status)),
            signal: None,
        }
    }
}

/// True while `pid` is a live process leading its own process group.
fn is_group_leader(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: getpgid only queries; it returns -1 for a pid that is gone.
    pid > 1 && unsafe { libc::getpgid(pid) } == pid
}

/// Send `signal` to the process group `pgid`; with signal 0, only ask
/// whether the group has members. True when the group was there.
fn signal_group(pgid: u32, signal: libc::c_int) -> bool {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return false;
    };
    // Group 0 is our own and group 1 is init's.
    if pgid <= 1 {
        return false;
    }
    // SAFETY: a negative pid addresses the process group; range checked
    // above.
    unsafe { libc::kill(-pgid, signal) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redraw_always_has_another_size_to_go_to() {
        let size = |cols, rows| Size {
            cols,
            rows,
            pixel_width: 720,
            pixel_height: 432,
        };
        let cells = |size: Size| (size.cols, size.rows);
        assert_eq!(cells(size(80, 24).nudged()), (80, 23));
        // One row cannot lose a row, and one cell cannot lose anything.
        assert_eq!(cells(size(80, 1).nudged()), (79, 1));
        assert_eq!(cells(size(1, 1).nudged()), (1, 2));
        // The pixels are the window's and stay.
        assert_eq!(size(80, 24).nudged().pixel_height, 432);
    }
}
