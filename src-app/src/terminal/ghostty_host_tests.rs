//! Tests for a Ghostty terminal whose PTY is in the session host.
//!
//! A real host on a temporary socket and real children, as for the other
//! terminal: the host, the programs and the waits are shared with those
//! tests. A child module of `pty_session`, so that it can build a pane the
//! way the view does and look at what its terminal state holds. Nothing
//! here asserts by sleeping: every wait is a poll with a deadline.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use splitlane_host::{Facts, Rgb, SessionId, SessionInfo, SpawnSpec};

use super::host_test_support::*;
use super::*;
use crate::terminal::ghostty_session::{GhosttyAttach, GhosttySession, SpawnedGhostty};
use crate::terminal::host_session::{
    HostEndpoint, OpenError, Opened, ReattachError, attach_or_open, open_or, open_remote, reattach,
    stop_all,
};

fn facts() -> Facts {
    crate::terminal::ghostty_session::window_facts(&crate::theme::active_theme(), None)
}

/// A terminal state with a Ghostty pane that has no runtime yet, as the
/// view makes one, and what a session in the host is attached to.
fn pending_pane(cols: usize, rows: usize) -> (TerminalState, GhosttyAttach) {
    let (mut state, _alacritty) = TerminalState::new_pending(cols, rows);
    let (session, pending, events) = GhosttySession::pending_with_clipboard_gate(
        TerminalWindowSize::new(cols, rows, 0, 0),
        state.clipboard_gate(),
    );
    state.attach_ghostty(session.clone(), events);
    state.set_terminal_focused(true);
    (
        state,
        GhosttyAttach {
            session,
            pending,
            max_scrollback: 1000,
        },
    )
}

/// Give a pane a PTY the window owns, as the view does when the host is
/// not used.
fn start_in_window(params: SpawnParams, target: GhosttyAttach) -> SpawnedGhostty {
    target
        .session
        .start(target.pending, params, None, target.max_scrollback)
        .unwrap_or_else(|error| unreachable!("a PTY the window owns: {error}"))
}

/// A pane opened in the host, the way the view opens one.
fn pane_in_host(host: &TestHost, params: SpawnParams) -> TerminalState {
    let (mut state, target) = pending_pane(params.cols, params.rows);
    match open_or(
        Some(&host.endpoint),
        facts(),
        params,
        target,
        PANE,
        start_in_window,
    ) {
        Opened::InHost(spawned) => state.promote_remote_ghostty(spawned),
        Opened::InWindow(_) => unreachable!("the host did not take the pane"),
        Opened::Lost(e) => unreachable!("the pane lost its shell: {e:#}"),
    }
    state
}

/// A pane with a PTY the window owns, for comparison.
fn pane_in_window(params: SpawnParams) -> TerminalState {
    let (mut state, target) = pending_pane(params.cols, params.rows);
    state.promote_ghostty(start_in_window(params, target));
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

/// The grid as the window draws it, row by row. Unlike [`text`] this can
/// still be asked of a pane whose child has ended: the runtime that would
/// answer for the text stops with the child, and what it last published
/// stays.
fn drawn(state: &TerminalState) -> String {
    let (content, _) =
        state
            .session_backend()
            .render_content(TerminalWindowSize::new(1, 1, 0, 0), 0, 0, false);
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
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pump the pane until it knows its child ended, and return the code and
/// what the pane shows then.
fn wait_for_exit(state: &mut TerminalState) -> (i32, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        state.sync();
        if let Some(code) = state.exited {
            return (code, drawn(state));
        }
        assert!(
            Instant::now() < deadline,
            "the pane never saw its child end:\n{}",
            drawn(state)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn session_of(state: &TerminalState) -> SessionId {
    state.remote.as_ref().expect("a pane in the host").session()
}

fn the_session(host: &TestHost, what: &str, ready: impl Fn(&SessionInfo) -> bool) -> SessionInfo {
    wait_for(what, || {
        host.sessions()
            .into_iter()
            .next()
            .filter(|info| ready(info))
    })
}

/// A session nobody's pane started.
fn a_session_running(host: &TestHost, script: &str) -> (SessionId, u32) {
    host.client()
        .spawn(SpawnSpec {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: vec![("PATH".into(), "/bin:/usr/bin".into())],
            cwd: "/".into(),
            cols: 80,
            rows: 24,
            ..SpawnSpec::default()
        })
        .expect("a session")
}

#[test]
fn a_pane_in_the_host_shows_the_programs_output_and_takes_input() {
    let host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, pane_params("/bin/sh", &[]));
    assert!(pane.is_in_host());

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
    assert_eq!(pane.host_session_id(), Some(session.id.to_string()));
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
fn input_written_before_the_shell_exists_reaches_it_in_the_order_it_was_written() {
    let host = TestHost::start(|_| {});
    let params = sh("read first; read second; echo \"got $first then $second\"; sleep 300");
    let (mut pane, target) = pending_pane(params.cols, params.rows);
    // An agent's launch command is written the moment its pane mounts,
    // and a person may be typing behind it.
    pane.write_to_pty(b"EARLY\n".to_vec());
    pane.write_to_pty(b"LATER\n".to_vec());
    let spawned = match open_remote(&params, &target, &host.endpoint, facts(), PANE) {
        Ok(spawned) => spawned,
        Err(OpenError::BeforeSpawn(e) | OpenError::AfterSpawn(e)) => {
            unreachable!("the pane was not opened in the host: {e:#}")
        }
    };
    pane.promote_remote_ghostty(spawned);
    wait_for_text(&mut pane, "got EARLY then LATER");
}

/// A shell must not be able to tell from its environment where its PTY
/// lives. `env` itself is the program, because a shell in between adds
/// variables of its own.
#[test]
fn a_shell_in_the_host_gets_the_environment_a_shell_in_the_window_gets() {
    let host = TestHost::start(|_| {});
    // Wide, so that no value is wrapped over two rows, and tall, so that
    // every variable is still on the grid when the program has ended.
    let wide = || {
        let mut params = pane_params("/usr/bin/env", &[]);
        (params.cols, params.rows) = (1024, 200);
        params
    };
    let environment = |pane: &mut TerminalState| {
        let (code, shown) = wait_for_exit(pane);
        assert_eq!(code, 0, "{shown}");
        let mut lines: Vec<String> = shown
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        lines.sort_unstable();
        lines
    };
    let in_window = environment(&mut pane_in_window(wide()));
    let in_host = environment(&mut pane_in_host(&host, wide()));

    assert!(in_host.len() > 5, "{in_host:?}");
    for expected in [
        "TERM=xterm-256color",
        "TERM_PROGRAM=ghostty",
        "COLORTERM=truecolor",
        "SPLITLANE_SURFACE_ID=11",
        "SPLITLANE_WORKSPACE_ID=7",
    ] {
        assert!(in_host.iter().any(|line| line == expected), "{expected}");
    }
    assert!(
        in_host
            .iter()
            .any(|line| line.starts_with("TERM_PROGRAM_VERSION="))
    );
    assert!(in_host.iter().any(|line| line.starts_with("SHELL=/")));
    // No variable is exempt: nothing differs legitimately.
    assert_eq!(in_host, in_window);
}

/// A shell whose directory is not there starts where the PTY library would
/// have started it.
#[test]
fn a_shell_whose_directory_is_gone_starts_where_a_shell_in_the_window_does() {
    let host = TestHost::start(|_| {});
    let lost = || {
        let mut params = pane_params("/bin/pwd", &[]);
        params.cwd = PathBuf::from("/no/such/directory/anywhere");
        params
    };
    let (_, in_window) = wait_for_exit(&mut pane_in_window(lost()));
    let (_, in_host) = wait_for_exit(&mut pane_in_host(&host, lost()));
    assert!(in_host.trim().starts_with('/'), "{in_host}");
    assert_eq!(in_host.trim(), in_window.trim());
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
    assert_eq!(pane.session_backend().grid_metrics().columns, 100);
}

/// What a font zoom is to the PTY: the same grid with cells of another
/// size. The program is signalled about it, in the host as in a pane the
/// window owns.
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

    wait_for_text(&mut pane, " first;");
    // Exactly the two answers, and nothing behind them: a second answer
    // from the pane's own terminal, which read the same questions, would
    // be between the brackets.
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
    wait_for_text(&mut pane, " second;");
    assert!(
        on_one_line(&pane).contains(&format!("{} more[] second;", hex(&second))),
        "{}",
        text(&pane)
    );
    assert_eq!(pane.refused_input_count(), 0);
    assert_eq!(host.sessions()[0].dropped_replies, 0);
}

#[test]
fn how_a_child_ended_reaches_the_pane_after_its_last_output() {
    let host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, sh("printf 'last words'; exit 3"));
    let (code, shown) = wait_for_exit(&mut pane);
    assert_eq!(code, 3);
    assert_eq!(pane.exit_signal, None);
    // On the grid by the time the exit is known.
    assert!(shown.contains("last words"), "{shown:?}");
    // An exit with a code and no input from anybody: a pane that stays
    // open to show the code, as for a shell that failed to start.
    assert!(!pane.should_close_on_exit());

    // A child that was killed, as a pane that owns its PTY reports one.
    let killing = || sh("printf 'about to go'; kill -TERM $$");
    let mut killed = pane_in_host(&host, killing());
    let (code, _) = wait_for_exit(&mut killed);
    let mut killed_in_window = pane_in_window(killing());
    let (code_in_window, _) = wait_for_exit(&mut killed_in_window);
    assert_eq!(code, code_in_window);
    assert_eq!(killed.exit_signal, killed_in_window.exit_signal);
    assert_eq!(killed.exit_signal.as_deref(), Some("Terminated"));

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

/// A pane closed while its shell was being started is gone by the time the
/// shell is there, and what was started is dropped without ever having been
/// the pane's.
#[test]
fn a_shell_started_for_a_pane_that_is_gone_is_ended() {
    let host = TestHost::start(|_| {});
    let params = sh("sleep 300");
    let (state, target) = pending_pane(params.cols, params.rows);
    let spawned = match open_or(
        Some(&host.endpoint),
        facts(),
        params,
        target,
        PANE,
        start_in_window,
    ) {
        Opened::InHost(spawned) => spawned,
        Opened::InWindow(_) => unreachable!("the host did not take the pane"),
        Opened::Lost(e) => unreachable!("the pane lost its shell: {e:#}"),
    };
    let pid = spawned.child_pid;
    assert!(group_alive(pid));

    drop(state);
    drop(spawned);
    wait_for("the process group to be gone", || {
        (!group_alive(pid)).then_some(())
    });
    wait_for("the host to forget the session", || {
        host.sessions().is_empty().then_some(())
    });
}

/// Input a program does not read is not taken off the pane's connection by
/// the host. The pane says no at its limit, to whoever writes, and goes on
/// showing its session; closing it does not wait behind what is stuck.
#[test]
fn input_a_program_does_not_read_is_refused_at_the_limit_and_the_pane_can_still_be_closed() {
    let host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, sh("stty raw -echo; echo deaf; sleep 300"));
    wait_for_text(&mut pane, "deaf");
    let pid = pane.child_pid;

    // Until the terminal, the host's queue for it and the socket hold no
    // more: then the connection is held up, the pane's own queue stops
    // emptying, and at its limit the pane says no.
    let chunk = vec![b'x'; 4 * 1024];
    wait_for("the pane to refuse input", || {
        pane.write_to_pty(chunk.clone());
        pane.sync();
        (pane.refused_input_count() > 0).then_some(())
    });
    // Refusing is all it did: the pane has not ended.
    pane.sync();
    assert_eq!(pane.exited, None);
    assert!(group_alive(pid));

    let closed_at = Instant::now();
    drop(pane);
    wait_for("the process group to be gone", || {
        (!group_alive(pid)).then_some(())
    });
    assert!(
        closed_at.elapsed() < Duration::from_secs(10),
        "closing the pane waited for the program"
    );
}

/// A pane of a window that starts after the one `host.endpoint` stands for
/// is gone: another endpoint on the same host, and a pane with another id.
fn pane_of_the_next_window(
    host: &TestHost,
    session: SessionId,
    surface: u64,
) -> (Arc<HostEndpoint>, Result<TerminalState, ReattachError>) {
    let window = HostEndpoint::new(host.socket.clone(), None);
    let (mut state, target) = pending_pane(80, 24);
    let attached = reattach(&window, session, &target, facts(), surface).map(|spawned| {
        state.promote_remote_ghostty(spawned);
        state
    });
    (window, attached)
}

#[test]
fn a_window_that_goes_away_leaves_its_sessions_running_and_the_next_one_finds_them() {
    let host = TestHost::start(|_| {});
    // The shell is told an id for its pane, as every shell in the host is.
    // Ids of this test's own, because the record of what an id means is
    // the process's and other tests run beside this one.
    let (first_pane, told, next_pane) = (881_000, 881_001, 881_002);
    let mut params = TerminalState::resolve_spawn_params(
        Some(PathBuf::from("/")),
        7,
        told,
        Some((80, 24)),
        None,
    );
    params.shell = "/bin/sh".to_string();
    params.extra_args = vec![
        "-c".to_string(),
        "printf '\\033]0;kept-title\\007'; echo before-$SPLITLANE_SURFACE_ID; read line; \
         echo after-$line; sleep 300"
            .to_string(),
    ];
    let (mut pane, target) = pending_pane(80, 24);
    match open_or(
        Some(&host.endpoint),
        facts(),
        params,
        target,
        first_pane,
        start_in_window,
    ) {
        Opened::InHost(spawned) => pane.promote_remote_ghostty(spawned),
        _ => unreachable!("the host did not take the pane"),
    }
    wait_for_text(&mut pane, "before-881001");
    let shown_before = text(&pane);
    let saved = pane
        .host_session_id()
        .expect("a pane in the host has a session");
    let session: SessionId = saved.parse().expect("the id a layout is saved with");
    assert_eq!(crate::surface_alias::pane_meant(told), first_pane);

    // The window quits with its sessions to be kept: the pane is torn
    // down, and its session is not ended.
    host.endpoint.leave_sessions_running();
    drop(pane);
    let left = the_session(&host, "the pane to let go of its session", |info| {
        info.attached == 0
    });
    assert_eq!(left.id, session);
    assert_eq!(left.exited, None, "the session was ended with its window");
    // Nothing means the pane that is gone.
    wait_for("the old pane's id to be free", || {
        (crate::surface_alias::pane_meant(told) == told).then_some(())
    });

    // The next window lists what the host holds and finds it.
    let window = HostEndpoint::new(host.socket.clone(), None);
    let held = window.held_sessions();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, saved);
    assert_eq!(held[0].exported_surface, Some(told));
    assert_eq!(held[0].exported_workspace, Some(7));

    // And shows it in a pane of its own: the same screen, the same shell,
    // and what its program says of its pane now means the new pane.
    let (_window, attached) = pane_of_the_next_window(&host, session, next_pane);
    let mut pane = attached.expect("the session is still there");
    wait_for_text(&mut pane, "before-881001");
    assert_eq!(text(&pane), shown_before);
    // What the log said last of itself is known once it has been replayed.
    pane.sync();
    assert_eq!(pane.title, "kept-title");
    assert_eq!(pane.child_pid, left.pid);
    assert_eq!(pane.host_session_id(), Some(saved));
    assert_eq!(crate::surface_alias::pane_meant(told), next_pane);
    // The first layout of the new window gives the session the window's
    // size and does not wipe what was replayed, although a view that
    // lays a pane out for the first time asks for exactly that.
    let (_, asked) =
        pane.session_backend()
            .render_content(TerminalWindowSize::new(90, 20, 9, 18), 0, 19, true);
    assert!(asked);
    wait_for("the host to confirm the size", || {
        (pane.kernel_window_size() == Some((90, 20))).then_some(())
    });
    assert!(text(&pane).contains("before-881001"), "{}", text(&pane));
    pane.write_to_pty(b"typed\n".as_slice());
    wait_for_text(&mut pane, "after-typed");

    // Closing that pane is still an ending.
    drop(pane);
    wait_for("the host to forget the session", || {
        host.sessions().is_empty().then_some(())
    });
    wait_for("the new pane's id to be free", || {
        (crate::surface_alias::pane_meant(told) == told).then_some(())
    });
}

#[test]
fn a_session_that_ended_while_no_window_was_there_is_not_attached_to() {
    let host = TestHost::start(|_| {});
    let (session, _) = a_session_running(&host, "echo done");
    the_session(&host, "the session to end", |info| info.exited.is_some());

    // The pane is told the session is gone before anything of the pane
    // was touched, so it can be given a new shell: the view's own path
    // ends with a shell, in the host, and says the pane is a new one.
    let (mut state, target) = pending_pane(80, 24);
    let settled = attach_or_open(
        Some(&host.endpoint),
        Some(session),
        facts(),
        sh("echo a-new-shell; sleep 300"),
        target,
        881_011,
        start_in_window,
    );
    assert!(!settled.attached_to_held);
    assert!(!settled.held_unanswered);
    match settled.opened {
        Opened::InHost(spawned) => state.promote_remote_ghostty(spawned),
        _ => unreachable!("the pane was not given a new shell in the host"),
    }
    wait_for_text(&mut state, "a-new-shell");
    assert_ne!(session_of(&state), session);
    // And the host no longer holds the ended one's log for a reader that
    // will not come.
    assert_eq!(host.sessions().len(), 1);
    drop(state);

    // The same for a session the host has never heard of, and for a host
    // that is not there.
    let (_window, attached) = pane_of_the_next_window(&host, SessionId::new_random(), 881_012);
    assert!(matches!(attached, Err(ReattachError::Gone(_))));
    let nowhere = HostEndpoint::new(short_dir().path().join("no.sock"), None);
    let (_state, target) = pending_pane(80, 24);
    assert!(matches!(
        reattach(&nowhere, session, &target, facts(), 881_013),
        Err(ReattachError::Gone(_))
    ));
    assert!(nowhere.held_sessions().is_empty());
}

/// A host that is there and does not answer for a session has not said
/// the session is gone. The pane must not take that for leave to start a
/// new shell and resume the agent in it: the first agent may still be at
/// work on the same conversation.
#[test]
fn a_host_that_does_not_answer_for_a_session_starts_nothing() {
    // Room for one connection: the window's own takes it, and the pane's
    // is turned away.
    let host = TestHost::start(|config| config.max_connections = 1);
    let (session, pid) = a_session_running(&host, "sleep 300");
    let settled = wait_for("the host to have room for the window", || {
        let window = HostEndpoint::new(host.socket.clone(), None);
        let (state, target) = pending_pane(80, 24);
        let settled = attach_or_open(
            Some(&window),
            Some(session),
            facts(),
            sh("echo must-not-run; sleep 300"),
            target,
            881_021,
            // Turned away at the first connection too while the one that
            // started the session is still being let go of. That is a
            // host that is not there, the pane is the window's to open,
            // and the test asks again.
            |_, _| (),
        );
        drop(window);
        match &settled.opened {
            Opened::InHost(spawned) if !settled.attached_to_held => {
                unreachable!(
                    "session {}: a second shell was started in the host",
                    spawned.handle.session()
                )
            }
            _ => {}
        }
        settled.held_unanswered.then_some((settled, state))
    });
    let (settled, state) = settled;
    // The pane keeps being what it was saved as, and has an error to show.
    assert!(settled.attached_to_held);
    let Opened::Lost(error) = settled.opened else {
        unreachable!("the pane was given something after all");
    };
    assert!(
        format!("{error:#}").contains("may still be running"),
        "{error:#}"
    );
    // Nothing was started for it: no runtime, no shell, no session.
    assert_eq!(state.child_pid, 0);
    assert!(!state.is_in_host());
    assert!(group_alive(pid));
    let held = wait_for("the session to be listed again", || {
        let held = HostEndpoint::new(host.socket.clone(), None).held_sessions();
        (!held.is_empty()).then_some(held)
    });
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, session.to_string());
    wait_for("the test's own session to be ended", || {
        host.client().kill(session).ok()
    });
}

#[test]
fn leaving_with_the_sessions_to_be_stopped_ends_everything_the_host_holds() {
    let mut host = TestHost::start(|_| {});
    let mut shown = pane_in_host(&host, sh("echo shown; sleep 300"));
    wait_for_text(&mut shown, "shown");
    let shown_pid = shown.child_pid;
    // One that no pane of this window shows: it is stopped too.
    let (_, unshown_pid) = a_session_running(&host, "sleep 300");

    stop_all(&host.endpoint, Duration::from_secs(10));
    // The pane is torn down after that, as the window's are, and must not
    // hold anything up.
    drop(shown);
    let thread = host.thread.take().expect("the host's thread");
    wait_for("the host to exit", || thread.is_finished().then_some(()));
    thread
        .join()
        .expect("the host's thread")
        .expect("a clean exit");
    for pid in [shown_pid, unshown_pid] {
        // SAFETY: signal 0 only asks whether the process is there.
        assert!(
            unsafe { libc::kill(pid as libc::pid_t, 0) } != 0,
            "process {pid} outlived the host"
        );
    }
}

#[test]
fn a_host_that_cannot_be_reached_leaves_the_pane_to_the_window() {
    let dir = short_dir();
    let nowhere = HostEndpoint::new(dir.path().join("no.sock"), None);
    let (mut pane, target) = pending_pane(80, 24);
    let params = sh("echo in-the-window; sleep 300");
    match open_or(
        Some(&nowhere),
        facts(),
        params,
        target,
        PANE,
        start_in_window,
    ) {
        Opened::InWindow(spawned) => pane.promote_ghostty(spawned),
        _ => unreachable!("there is no host to take the pane"),
    }
    assert!(!pane.is_in_host());
    assert_eq!(pane.host_session_id(), None);
    // The pane is the one it would have been without the switch.
    wait_for_text(&mut pane, "in-the-window");
    // And the host is left alone for a while, so that a window restoring
    // many panes waits for it once.
    assert!(nowhere.is_avoided());
}

/// A pane that lost its connection says so and shows as ended, and its
/// session is not ended by that: the host may be fine.
#[test]
fn a_pane_whose_host_goes_away_says_so_and_ends() {
    let mut host = TestHost::start(|_| {});
    let mut pane = pane_in_host(&host, sh("echo working; sleep 300"));
    wait_for_text(&mut pane, "working");

    // The host stops with everything it holds: for the pane that is its
    // child ending, or the connection going first.
    assert!(host.client().shutdown(true).is_ok());
    let thread = host.thread.take().expect("the host's thread");
    wait_for("the host to exit", || thread.is_finished().then_some(()));
    let _ = thread.join();
    let (_, shown) = wait_for_exit(&mut pane);
    assert!(shown.contains("working"), "{shown}");
    assert!(pane.exited.is_some());
}
