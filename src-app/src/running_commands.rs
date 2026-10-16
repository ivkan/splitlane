//! What a terminal session is running besides its shell.
//!
//! The rail names a shell surface after its foreground command, but that name
//! comes from a process scan that runs in bursts of output and only knows a
//! program's name. Quitting, the Running sheet and `splitlane ps --all` need
//! more: whether a command is running **now**, its whole command line, and
//! how long it has been going.
//!
//! # What counts as a running command
//!
//! On Unix, the job the terminal is giving its keyboard to. An interactive
//! shell puts every job it starts in a process group of its own and hands
//! that group the terminal; when the job ends, the shell takes the terminal
//! back. So a command runs exactly while the terminal's foreground process
//! group is not the shell's. The kernel reports the foreground group of a
//! process's controlling terminal (`tpgid`) to anyone who may inspect the
//! process, which matters because a pane whose PTY lives in the session host
//! has no master in this process to ask `tcgetpgrp` of.
//!
//! Every process group among the shell's descendants was the other way to
//! read it, and it is the wrong one: prompt helpers (powerlevel10k's
//! `gitstatusd`, zsh async workers) sit in groups of their own for the whole
//! life of the shell, and the shell would never read as idle. The cost of the
//! foreground rule is that a job sent to the background with `&` is not
//! counted. A job stopped with Ctrl+Z is: the terminal is back with the
//! shell, but closing the session would still end the job, so it is listed
//! as stopped.
//!
//! An interactive program in the foreground (an editor, `less`, `ssh`) is a
//! running command too. Closing the session ends it, and losing an unsaved
//! file is exactly what the question at quit is for.
//!
//! A session whose PTY child is not a shell at all was started on a program
//! directly, and that program is its command.
//!
//! Windows has no process groups and no foreground group. There, every direct
//! child of the shell that is not the console's own helper is a command.

use crate::process_tree::ProcessSnapshot;

/// A command running in one terminal session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunningCommand {
    /// The process that names the command: the leader of its process group
    /// on Unix, the shell's child on Windows.
    pub(crate) pid: u32,
    /// Its command line, arguments joined by spaces; the program's name
    /// where the arguments cannot be read.
    pub(crate) command: String,
    /// When it started, in seconds since the Unix epoch. Only ever shown as
    /// a duration, so a clock step costs a wrong number and nothing else.
    pub(crate) started_unix: Option<i64>,
    /// Stopped with Ctrl+Z rather than running.
    pub(crate) stopped: bool,
    /// The agent CLI this command is, when its program is one: a session
    /// the host holds with no pane can be an agent's, and nothing else
    /// would say so.
    pub(crate) agent: Option<&'static str>,
}

/// The agent binary `command` runs, matched on its program's file name.
pub(crate) fn agent_of(command: &str, binaries: &[&'static str]) -> Option<&'static str> {
    let program = command.split_whitespace().next()?;
    let name = program.rsplit(['/', '\\']).next()?;
    let name = name.strip_suffix(".exe").unwrap_or(name);
    binaries.iter().copied().find(|binary| *binary == name)
}

/// The longest command line kept. A command line can be most of a megabyte,
/// and it is only ever shown on one line.
const MAX_COMMAND_LEN: usize = 1024;

/// The commands running under the session whose PTY child is `pty_child`,
/// with `snapshot` to find its descendants in. Empty when nothing runs, and
/// when the session itself cannot be read.
///
/// Without a snapshot the foreground job is still found - its leader is
/// named by the shell's own `tpgid` - and only stopped jobs are missed, so a
/// process table that could not be read does not turn a running dev server
/// into an idle shell. Windows needs the table and finds nothing without it.
///
/// Blocking: a handful of per-process reads. Call it off the render thread.
pub(crate) fn commands_under(
    snapshot: Option<&ProcessSnapshot>,
    pty_child: u32,
) -> Vec<RunningCommand> {
    if pty_child == 0 {
        return Vec::new();
    }
    commands_under_impl(snapshot, pty_child)
}

/// A command as a card names it: the program and up to two words after it,
/// stopping at the first flag, each without its directory - `npm run dev`,
/// `cargo build`, `python backfill.py`. Where it is asked "what is this" and
/// not "which one exactly".
pub(crate) fn short_command(command: &str) -> String {
    let base = |word: &str| word.rsplit(['/', '\\']).next().unwrap_or(word).to_string();
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return String::new();
    };
    let program = base(program);
    let program = program.strip_suffix(".exe").unwrap_or(&program).to_string();
    let mut out = vec![program];
    out.extend(
        words
            .take_while(|word| !word.starts_with('-'))
            .take(2)
            .map(base),
    );
    out.join(" ")
}

/// Stop the command led by `pid`: its process group gets `SIGTERM` on Unix,
/// the process is terminated on Windows. Returns whether the signal went.
///
/// The group is read now and checked to still hold `pid`, so a pid that was
/// reused since the census named it is not what gets the signal; this app's
/// own group, and the groups of init and the kernel, never do.
pub(crate) fn stop_command(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    stop_impl(pid)
}

#[cfg(unix)]
fn stop_impl(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: both are queries with no preconditions; `getpgid` answers -1
    // for a process that is gone.
    let (group, own) = unsafe { (libc::getpgid(pid), libc::getpgrp()) };
    if group <= 1 || group == own {
        return false;
    }
    // SAFETY: a signal to a group just read from a live member.
    unsafe { libc::kill(-group, libc::SIGTERM) == 0 }
}

#[cfg(windows)]
fn stop_impl(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: Win32 calls; a non-null handle is closed on every path out.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        ok
    }
}

#[cfg(not(any(unix, windows)))]
fn stop_impl(_pid: u32) -> bool {
    false
}

fn is_shell(name: &str) -> bool {
    crate::workspace::surface_naming::is_shell_program(name) || {
        let name = name.to_ascii_lowercase();
        name == "cmd" || name == "cmd.exe"
    }
}

fn clip(mut command: String) -> String {
    if command.len() > MAX_COMMAND_LEN {
        let mut end = MAX_COMMAND_LEN;
        while !command.is_char_boundary(end) {
            end -= 1;
        }
        command.truncate(end);
    }
    command
}

/// Arguments as the kernel stores them, NUL-separated, joined by spaces.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn join_args(raw: &[u8]) -> Option<String> {
    let args: Vec<String> = raw
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    (!args.is_empty()).then(|| clip(args.join(" ")))
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

/// What one process says about itself that this module needs.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcInfo {
    name: String,
    pgid: i32,
    /// The foreground process group of the process's controlling terminal;
    /// zero or negative when it has none.
    tpgid: i32,
    stopped: bool,
    started_unix: Option<i64>,
}

/// The rule, over facts already read: the root first, then its descendants
/// in breadth-first order. Split from the reads so it is tested on every
/// platform.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn commands_from(
    root: (u32, &ProcInfo),
    descendants: &[(u32, ProcInfo)],
    command_line: &dyn Fn(u32, &ProcInfo) -> String,
) -> Vec<RunningCommand> {
    let (root_pid, root_info) = root;
    let named = |pid: u32, info: &ProcInfo| RunningCommand {
        pid,
        command: command_line(pid, info),
        started_unix: info.started_unix,
        stopped: info.stopped,
        agent: None,
    };
    if !is_shell(&root_info.name) {
        return vec![named(root_pid, root_info)];
    }
    // The group's leader names it when it is still there; a group whose
    // leader has exited is named by its first member found.
    let leader_of = |group: i32| {
        descendants
            .iter()
            .find(|(pid, _)| i64::from(*pid) == i64::from(group))
            .or_else(|| descendants.iter().find(|(_, info)| info.pgid == group))
    };
    let mut out = Vec::new();
    let mut groups = Vec::new();
    let foreground = root_info.tpgid;
    if foreground > 0
        && foreground != root_info.pgid
        && let Some((pid, info)) = leader_of(foreground)
    {
        out.push(named(*pid, info));
        groups.push(foreground);
    }
    for (_, info) in descendants {
        if info.stopped
            && info.pgid > 1
            && info.pgid != root_info.pgid
            && !groups.contains(&info.pgid)
        {
            groups.push(info.pgid);
            if let Some((pid, leader)) = leader_of(info.pgid) {
                out.push(RunningCommand {
                    stopped: true,
                    ..named(*pid, leader)
                });
            }
        }
    }
    out
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn commands_under_impl(snapshot: Option<&ProcessSnapshot>, pty_child: u32) -> Vec<RunningCommand> {
    let Some(root) = proc_info(pty_child) else {
        return Vec::new();
    };
    let mut descendants: Vec<(u32, ProcInfo)> = if is_shell(&root.name) {
        snapshot
            .and_then(|snapshot| snapshot.descendants_of(pty_child))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(pid, _)| proc_info(pid).map(|info| (pid, info)))
            .collect()
    } else {
        Vec::new()
    };
    // The foreground group's leader, asked about directly when the walk did
    // not reach it: the table may be missing or overrun.
    if is_shell(&root.name)
        && root.tpgid > 0
        && root.tpgid != root.pgid
        && let Ok(leader) = u32::try_from(root.tpgid)
        && !descendants.iter().any(|(pid, _)| *pid == leader)
        && let Some(info) = proc_info(leader)
    {
        descendants.insert(0, (leader, info));
    }
    commands_from((pty_child, &root), &descendants, &|pid, info| {
        command_line(pid).unwrap_or_else(|| info.name.clone())
    })
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn proc_info(pid: u32) -> Option<ProcInfo> {
    // Both are fixed for as long as the machine is up, so they are read once.
    static CLOCK: std::sync::OnceLock<(Option<i64>, Option<i64>)> = std::sync::OnceLock::new();
    let (boot, ticks) = *CLOCK.get_or_init(|| (boot_time(), clock_ticks()));
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&stat, boot, ticks)
}

/// One `/proc/<pid>/stat` line. The name is in parentheses and may itself
/// hold spaces and parentheses, so the fields are counted from the last `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_stat(stat: &str, boot_time: Option<i64>, ticks: Option<i64>) -> Option<ProcInfo> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    // Fields from 3 on: state, ppid, pgrp, session, tty_nr, tpgid, ... and
    // starttime is field 22.
    let fields: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    let state = fields.first()?;
    if state.starts_with('Z') {
        return None;
    }
    let pgid = fields.get(2)?.parse().ok()?;
    let tpgid = fields.get(5)?.parse().ok()?;
    let started_unix = fields
        .get(19)
        .and_then(|start| start.parse::<i64>().ok())
        .zip(boot_time.zip(ticks).filter(|(_, ticks)| *ticks > 0))
        .map(|(start, (boot, ticks))| boot + start / ticks);
    Some(ProcInfo {
        name,
        pgid,
        tpgid,
        stopped: state.starts_with('T'),
        started_unix,
    })
}

#[cfg(target_os = "linux")]
fn boot_time() -> Option<i64> {
    std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|secs| secs.trim().parse().ok())
}

#[cfg(target_os = "linux")]
fn clock_ticks() -> Option<i64> {
    // SAFETY: `sysconf` is a query with no preconditions.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    (ticks > 0).then_some(ticks as i64)
}

#[cfg(target_os = "linux")]
fn command_line(pid: u32) -> Option<String> {
    use std::io::Read;
    let mut raw = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .take(MAX_COMMAND_LEN as u64 * 4)
        .read_to_end(&mut raw)
        .ok()?;
    join_args(&raw)
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

/// `SSTOP`, the `pbi_status` of a stopped process.
#[cfg(target_os = "macos")]
const SSTOP: u32 = 4;

#[cfg(target_os = "macos")]
fn proc_info(pid: u32) -> Option<ProcInfo> {
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::proc_pid::pidinfo;

    let info = pidinfo::<BSDInfo>(i32::try_from(pid).ok()?, 0).ok()?;
    let comm: Vec<u8> = info
        .pbi_comm
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    Some(ProcInfo {
        name: String::from_utf8_lossy(&comm).into_owned(),
        pgid: i32::try_from(info.pbi_pgid).ok()?,
        tpgid: i32::try_from(info.e_tpgid).unwrap_or(-1),
        stopped: info.pbi_status == SSTOP,
        started_unix: i64::try_from(info.pbi_start_tvsec).ok(),
    })
}

/// `KERN_PROCARGS2`: the argument count, the executable's path, padding,
/// then the arguments, each NUL-terminated, then the environment.
#[cfg(target_os = "macos")]
fn command_line(pid: u32) -> Option<String> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut len: libc::size_t = 0;
    // SAFETY: a size query; `mib` has the three elements named.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len < 4 {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` has `len` writable bytes that outlive the call.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    parse_procargs(&buf)
}

#[cfg(any(target_os = "macos", test))]
fn parse_procargs(buf: &[u8]) -> Option<String> {
    let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let rest = buf.get(4..)?;
    // Skip the executable path and the NULs that pad it.
    let path_end = rest.iter().position(|byte| *byte == 0)?;
    let rest = &rest[path_end..];
    let start = rest.iter().position(|byte| *byte != 0)?;
    let mut args = Vec::new();
    let mut at = start;
    let mut taken = 0;
    while taken < argc && at < rest.len() {
        taken += 1;
        let end = rest[at..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(rest.len(), |len| at + len);
        args.extend_from_slice(&rest[at..end]);
        args.push(0);
        at = end + 1;
    }
    join_args(&args)
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// The console's own processes, which sit under a shell without being
/// anything the person started.
#[cfg(windows)]
const CONSOLE_HELPERS: &[&str] = &["conhost", "openconsole"];

#[cfg(windows)]
fn commands_under_impl(snapshot: Option<&ProcessSnapshot>, pty_child: u32) -> Vec<RunningCommand> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let name_of = |pid: u32| {
        crate::process_tree::exe_path_of(pid).and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
    };
    let named = |pid: u32, name: String| RunningCommand {
        pid,
        command: clip(name),
        started_unix: crate::process_tree::start_token(pid).and_then(filetime_to_unix),
        stopped: false,
        agent: None,
    };
    let Some(root) = name_of(pty_child) else {
        return Vec::new();
    };
    if !is_shell(&root) {
        return vec![named(pty_child, root)];
    }
    snapshot
        .children_of(pty_child)
        .iter()
        .filter_map(|&pid| {
            let name = name_of(pid)?;
            let helper = CONSOLE_HELPERS
                .iter()
                .any(|helper| name.eq_ignore_ascii_case(helper));
            (!helper).then(|| named(pid, name))
        })
        .collect()
}

/// A `FILETIME` - 100 ns intervals since 1601 - in Unix seconds.
#[cfg(any(windows, test))]
fn filetime_to_unix(filetime: u64) -> Option<i64> {
    const UNIX_EPOCH_IN_FILETIME_SECS: i64 = 11_644_473_600;
    i64::try_from(filetime / 10_000_000)
        .ok()
        .map(|secs| secs - UNIX_EPOCH_IN_FILETIME_SECS)
        .filter(|secs| *secs > 0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn commands_under_impl(
    _snapshot: Option<&ProcessSnapshot>,
    _pty_child: u32,
) -> Vec<RunningCommand> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, pgid: i32, tpgid: i32) -> ProcInfo {
        ProcInfo {
            name: name.to_string(),
            pgid,
            tpgid,
            stopped: false,
            started_unix: Some(1_000),
        }
    }

    fn names(pid: u32, info: &ProcInfo) -> String {
        format!("{}#{pid}", info.name)
    }

    #[test]
    fn a_shell_at_its_prompt_runs_nothing() {
        let shell = info("zsh", 100, 100);
        // A prompt helper in a group of its own is not a command.
        let helper = (140, info("gitstatusd", 140, 100));
        assert!(commands_from((100, &shell), &[helper], &names).is_empty());
    }

    #[test]
    fn the_foreground_group_is_the_command_named_by_its_leader() {
        let shell = info("zsh", 100, 200);
        let descendants = [
            (140, info("gitstatusd", 140, 100)),
            (201, info("node", 200, 200)),
            (200, info("npm", 200, 200)),
        ];
        let found = commands_from((100, &shell), &descendants, &names);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 200);
        assert_eq!(found[0].command, "npm#200");
        assert!(!found[0].stopped);
    }

    #[test]
    fn a_group_whose_leader_exited_is_named_by_a_member() {
        let shell = info("bash", 100, 200);
        let found = commands_from((100, &shell), &[(205, info("cc1", 200, 200))], &names);
        assert_eq!(found[0].pid, 205);
    }

    #[test]
    fn a_stopped_job_counts_although_the_shell_has_the_terminal() {
        let shell = info("zsh", 100, 100);
        let mut vim = info("vim", 300, 100);
        vim.stopped = true;
        let found = commands_from((100, &shell), &[(300, vim)], &names);
        assert_eq!(found.len(), 1);
        assert!(found[0].stopped);
    }

    #[test]
    fn a_session_started_on_a_program_is_that_program() {
        let program = info("htop", 100, 100);
        let found = commands_from((100, &program), &[], &names);
        assert_eq!(found[0].command, "htop#100");
    }

    #[test]
    fn a_login_shell_is_a_shell() {
        let shell = info("-zsh", 100, 100);
        assert!(commands_from((100, &shell), &[], &names).is_empty());
    }

    #[test]
    fn a_stat_line_is_read_from_its_last_parenthesis() {
        let line = "4242 (my (odd) prog) S 1 4242 4242 34817 4300 4194304 0 0 0 0 0 0 0 0 20 0 1 0 500 0 0";
        let info = parse_stat(line, Some(1_000_000), Some(100)).expect("parsed");
        assert_eq!(info.name, "my (odd) prog");
        assert_eq!(info.pgid, 4242);
        assert_eq!(info.tpgid, 4300);
        assert_eq!(info.started_unix, Some(1_000_005));
        assert!(!info.stopped);
        let zombie = "7 (gone) Z 1 7 7 0 -1 0";
        assert!(parse_stat(zombie, None, None).is_none());
    }

    #[test]
    fn procargs_skip_the_path_and_its_padding() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/bin/cargo\0\0\0\0cargo\0build\0--release\0HOME=/x\0");
        assert_eq!(
            parse_procargs(&buf).as_deref(),
            Some("cargo build --release")
        );
    }

    #[test]
    fn a_filetime_is_read_as_unix_seconds() {
        // 2024-01-01T00:00:00Z.
        assert_eq!(
            filetime_to_unix(133_485_408_000_000_000),
            Some(1_704_067_200)
        );
        assert_eq!(filetime_to_unix(0), None);
    }

    #[test]
    fn an_agent_is_known_by_its_program() {
        let binaries = ["claude", "codex"];
        assert_eq!(agent_of("claude --resume abc", &binaries), Some("claude"));
        assert_eq!(agent_of("/opt/bin/codex", &binaries), Some("codex"));
        assert_eq!(agent_of("C:\\tools\\codex.exe", &binaries), Some("codex"));
        assert_eq!(agent_of("npm run dev", &binaries), None);
        assert_eq!(agent_of("claude-code-cli", &binaries), None);
    }

    #[test]
    fn a_card_names_the_program_and_its_subcommand() {
        assert_eq!(short_command("npm run dev"), "npm run dev");
        assert_eq!(
            short_command("cargo build --release --features tls"),
            "cargo build"
        );
        assert_eq!(
            short_command("python scripts/backfill.py --since 2026-09"),
            "python backfill.py"
        );
        assert_eq!(short_command("/usr/bin/htop"), "htop");
        assert_eq!(
            short_command("C:\\tools\\node.exe server.js"),
            "node server.js"
        );
        assert_eq!(short_command(""), "");
    }

    #[cfg(unix)]
    #[test]
    fn a_stopped_command_is_gone() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("sleep");
        assert!(stop_command(child.id()));
        let status = child.wait().expect("wait");
        assert!(!status.success());
        // This process's own group is never signalled.
        assert!(!stop_command(std::process::id()));
    }

    #[test]
    fn a_long_command_line_is_clipped_on_a_character() {
        let long = "é".repeat(MAX_COMMAND_LEN);
        let clipped = clip(long);
        assert!(clipped.len() <= MAX_COMMAND_LEN);
        assert!(clipped.chars().all(|ch| ch == 'é'));
    }

    #[cfg(unix)]
    #[test]
    fn a_child_started_on_a_program_is_read_live() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        let snapshot = ProcessSnapshot::capture().expect("snapshot");
        let found = commands_under(Some(&snapshot), child.id());
        let without = commands_under(None, child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found, without);
        assert_eq!(found[0].command, "sleep 30");
        assert!(found[0].started_unix.is_some());
    }
}
