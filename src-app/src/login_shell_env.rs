//! Adopt the login shell's **PATH** (and `CLAUDE_CONFIG_DIR`) at startup.
//!
//! When Splitlane is launched from a `.desktop` entry / Dock / Finder, it
//! inherits the minimal systemd-user / launchd environment - the PATH is
//! missing Homebrew (`/opt/homebrew/bin`, `/usr/local/bin`), a Nix profile,
//! distro `/etc/profile.d` additions, and anything the user appended in their
//! login profile. Terminals opened inside that process then cannot find the
//! user's tools, and agent-CLI detection (`which::which("bunx")`) comes up
//! empty.
//!
//! We run the user's login shell once and adopt **two** variables from it:
//! `PATH`, for the reason above, and `CLAUDE_CONFIG_DIR`, which names the
//! directory Claude Code keeps its credentials in. The second one exists for
//! the limits footer (`claude_usage`): a GUI launch that cannot see the
//! override reads a *different* login's credentials than the sessions use, and
//! then reports somebody else's numbers or a permanent 401 - which is exactly
//! the shape of Nimbalyst's issue #975. It is adopted only when the process
//! does not already have one, so an explicitly-set variable always wins.
//!
//! We deliberately do NOT import the rest of the captured environment: a login
//! profile that re-exports session variables (`DISPLAY`, `WAYLAND_DISPLAY`,
//! `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `XAUTHORITY`, …) would
//! otherwise clobber the live values **before GPUI picks its X11/Wayland
//! backend and composes the IPC socket**, breaking the compositor / D-Bus /
//! clipboard connection. (Zed keeps the captured env in a side `HashMap`
//! applied only to PTYs/tasks; importing just PATH is the same idea with a
//! smaller surface, sufficient for the discovery problem this module exists to
//! solve.)
//!
//! Properties:
//! - **skipped on a terminal launch** (stdin is a TTY) - PATH was already
//!   inherited correctly;
//! - **portable** - uses POSIX `env` (not GNU `env -0`) so it works on
//!   BusyBox / Alpine, and falls back to `/bin/sh` for shells whose `-l -i -c`
//!   can't run the POSIX capture script (nushell, tcsh, xonsh, …); `/bin/sh`
//!   still sources `/etc/profile` + `/etc/profile.d` + `~/.profile`, i.e. the
//!   system PATH;
//! - **bounded** by a 5 s timeout so a pathological rc script can't wedge
//!   startup;
//! - **best-effort** - any failure logs and leaves the inherited PATH untouched.
//!
//! Safety: like [`crate::runtime_paths::augment_path_for_gui_launch`], this
//! mutates the process-global environment and MUST run on the main thread
//! before any other thread is spawned (Rust 2024 marks `set_var` `unsafe`). The
//! one helper thread it spawns to read stdout is always joined before the
//! `set_var`. On timeout the capture process group is killed and the reader
//! may be detached instead of wedging startup behind an inherited stdout FD.

#[cfg(not(unix))]
pub fn load_login_shell_env() {}

#[cfg(unix)]
pub fn load_login_shell_env() {
    use std::io::Read;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    // A terminal launch already inherited the login PATH from its parent shell
    // - skip the (~50-200 ms) re-capture. Only GUI launches (Finder / Dock /
    // `.desktop`) lack a controlling TTY on stdin.
    // SAFETY: `isatty` is a side-effect-free query on a file descriptor.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        return;
    }

    let user_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    // Only POSIX-family shells (and fish, which parses `printf …; exec env`)
    // understand the capture script. Exotic shells (nushell / tcsh / csh /
    // xonsh / elvish) reject `-l -i -c '<posix>'`, so capture with `/bin/sh` as
    // the login shell instead - it still sources `/etc/profile`,
    // `/etc/profile.d/*`, and `~/.profile`, which is where the system PATH
    // (Homebrew, Nix, distro additions) lives. We only need PATH, so that's
    // enough.
    let capture_shell = if is_posix_capture_shell(&user_shell) {
        user_shell.clone()
    } else {
        "/bin/sh".to_string()
    };

    // Print a unique marker (to skip rc-script chatter) then `exec env` so the
    // dump is the last thing on stdout. Plain POSIX `env` (NOT GNU `env -0`)
    // keeps this working on BusyBox / Alpine; we only read the `PATH=` line
    // afterwards, which is newline-free, so newline-delimited output is safe.
    const MARKER: &str = "__SPLITLANE_LOGIN_ENV_V2__";
    let script = format!("printf '%s\\n' '{MARKER}'; exec env");

    let mut cmd = Command::new(&capture_shell);
    cmd.arg("-l").arg("-i").arg("-c").arg(&script);
    if let Some(home) = dirs::home_dir() {
        // Spawn from $HOME - a sane cwd for a login shell. (We intentionally do
        // NOT prefix `cd` in the script: we only consume PATH, so per-directory
        // hooks like direnv/asdf/mise are irrelevant here.)
        cmd.current_dir(home);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and the only thing we do between
    // fork and exec. Putting the capture shell in its own session means a stray
    // rc script that opens `/dev/tty` can't grab our controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            log::debug!("login-shell env: could not spawn {capture_shell:?}: {e}");
            return;
        }
    };

    let Some(mut stdout) = child.stdout.take() else {
        terminate_login_shell_capture(&mut child);
        let _ = child.wait();
        return;
    };

    // Read stdout on a helper thread so the main thread can bound the wait. If
    // the shell wedges, the timeout fires, we kill it, and the reader unblocks
    // on EOF. The reader is joined before the `set_var` below, so no other
    // thread is touching the environment when we mutate it.
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let buf = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(buf) => {
            let _ = child.wait();
            let _ = reader.join();
            buf
        }
        Err(_) => {
            log::warn!(
                "login-shell env: {capture_shell:?} did not finish within 5s; keeping the inherited PATH"
            );
            terminate_login_shell_capture(&mut child);
            let _ = child.wait();
            if rx.recv_timeout(Duration::from_millis(250)).is_ok() {
                let _ = reader.join();
            } else {
                log::warn!(
                    "login-shell env: stdout reader stayed blocked after timeout; continuing startup"
                );
            }
            return;
        }
    };

    match extract_var(&buf, MARKER.as_bytes(), "PATH") {
        Some(path) if !path.is_empty() => {
            // SAFETY: main thread, before GPUI / any worker thread is spawned;
            // the reader thread was joined above. We import ONLY PATH and
            // CLAUDE_CONFIG_DIR - see the module doc for why adopting the full
            // login environment is unsafe.
            unsafe { std::env::set_var("PATH", &path) };
            log::info!(
                "login-shell env: adopted PATH from {capture_shell:?} ({} bytes)",
                path.len()
            );
        }
        _ => {
            log::warn!(
                "login-shell env: no PATH captured from {capture_shell:?} (unsupported shell or empty env); keeping the inherited PATH"
            );
        }
    }

    // The value is a path, not a secret, and an already-set one is never
    // overwritten: a launcher that set it deliberately outranks the profile.
    if std::env::var_os("CLAUDE_CONFIG_DIR").is_none()
        && let Some(dir) = extract_var(&buf, MARKER.as_bytes(), "CLAUDE_CONFIG_DIR")
        && !dir.is_empty()
    {
        // SAFETY: as above - still the main thread, reader joined.
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &dir) };
        log::info!("login-shell env: adopted CLAUDE_CONFIG_DIR from {capture_shell:?}");
    }
}

/// Shells whose `-l -i -c '<posix script>'` invocation runs our capture script.
/// fish parses `printf …; exec env` fine; nushell / tcsh / csh / xonsh / elvish
/// do not, and fall back to `/bin/sh`.
#[cfg(unix)]
fn is_posix_capture_shell(shell: &str) -> bool {
    let base = std::path::Path::new(shell)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(shell);
    matches!(
        base,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash" | "mksh" | "fish"
    )
}

#[cfg(unix)]
fn terminate_login_shell_capture(child: &mut std::process::Child) {
    let child_pid = child.id();
    if child_pid <= i32::MAX as u32 {
        let pgid = child_pid as libc::pid_t;
        // SAFETY: the child was spawned after `setsid`, so its PID is also the
        // process group id for normal rc-script descendants.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Extract `name`'s value from newline-delimited `env` output that follows
/// `marker`. Both variables we read are paths and neither can contain a
/// newline, so line-splitting is safe even when some other variable's value
/// spans multiple lines.
#[cfg(unix)]
fn extract_var(buf: &[u8], marker: &[u8], name: &str) -> Option<String> {
    let start = find_subslice(buf, marker)? + marker.len();
    let prefix = format!("{name}=");
    for line in buf[start..].split(|&b| b == b'\n') {
        if let Some(rest) = line.strip_prefix(prefix.as_bytes()) {
            return std::str::from_utf8(rest)
                .ok()
                .map(str::trim)
                .map(str::to_string);
        }
    }
    None
}

/// First index at which `needle` occurs in `haystack`. Tiny, allocation-free.
#[cfg(unix)]
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{extract_var, find_subslice, is_posix_capture_shell};

    #[test]
    fn find_subslice_locates_marker() {
        assert_eq!(find_subslice(b"junk__MARK__data", b"__MARK__"), Some(4));
        assert_eq!(find_subslice(b"__MARK__data", b"__MARK__"), Some(0));
        assert_eq!(find_subslice(b"no marker here", b"__MARK__"), None);
        assert_eq!(find_subslice(b"", b"__MARK__"), None);
        assert_eq!(find_subslice(b"data", b""), None);
    }

    #[test]
    fn extract_var_reads_the_named_line_after_marker() {
        let out = b"chatter\n__M__\nFOO=bar\nPATH=/a:/b:/c\nHOME=/h\n";
        assert_eq!(
            extract_var(out, b"__M__", "PATH").as_deref(),
            Some("/a:/b:/c")
        );
        assert_eq!(extract_var(out, b"__M__", "HOME").as_deref(), Some("/h"));
        // No marker -> None (we never read a PATH that precedes the marker).
        assert_eq!(extract_var(b"PATH=/x", b"__M__", "PATH"), None);
        // Marker present but no such line.
        assert_eq!(extract_var(b"__M__\nFOO=bar\n", b"__M__", "PATH"), None);
    }

    /// A name that is a suffix of another must not match it.
    #[test]
    fn extract_var_matches_the_whole_name() {
        let out = b"__M__\nXPATH=/wrong\nPATH=/right\n";
        assert_eq!(
            extract_var(out, b"__M__", "PATH").as_deref(),
            Some("/right")
        );
    }

    #[test]
    fn extract_var_reads_the_claude_config_dir() {
        let out = b"__M__\nPATH=/usr/bin\nCLAUDE_CONFIG_DIR=/Users/me/.claude-work\n";
        assert_eq!(
            extract_var(out, b"__M__", "CLAUDE_CONFIG_DIR").as_deref(),
            Some("/Users/me/.claude-work")
        );
    }

    #[test]
    fn extract_var_survives_multiline_var_before_it() {
        // A variable whose value contains a newline must not corrupt parsing.
        let out = b"__M__\nSCRIPT=line1\nline2\nPATH=/usr/bin\n";
        assert_eq!(
            extract_var(out, b"__M__", "PATH").as_deref(),
            Some("/usr/bin")
        );
    }

    #[test]
    fn is_posix_capture_shell_classifies() {
        for s in [
            "/bin/bash",
            "/usr/bin/zsh",
            "/bin/sh",
            "/usr/bin/fish",
            "dash",
        ] {
            assert!(is_posix_capture_shell(s), "{s} should be capturable");
        }
        for s in ["/usr/bin/nu", "/bin/tcsh", "/usr/bin/xonsh", "elvish"] {
            assert!(
                !is_posix_capture_shell(s),
                "{s} should fall back to /bin/sh"
            );
        }
    }
}
