//! Keeping file descriptors out of processes they were not meant for.
//!
//! A descriptor is supposed to be created close-on-exec, and on Linux it
//! is, in one step. macOS has no `pipe2`, no `accept4` and no
//! `SOCK_CLOEXEC`: there a descriptor exists for a moment without the flag,
//! and a child forked by another thread in that moment inherits it. What a
//! long-lived child inherits it keeps: an accepted connection that never
//! closes for its peer, a listener that still takes connections after its
//! owner is gone, or the pipe through which a launcher learns that some
//! other child has started, which then keeps that launcher waiting.
//!
//! So every process started from here marks whatever it inherited
//! close-on-exec before it execs, and a host closes whatever it was handed
//! before it opens anything of its own.

use std::os::fd::RawFd;

/// The standard streams end here.
const FIRST_INHERITED: RawFd = 3;

/// Upper bound on descriptor numbers in this process. To be called in the
/// parent: it is not async-signal-safe.
pub(crate) fn limit() -> RawFd {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limits` is a valid out-pointer for the call.
    let known = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } == 0;
    let soft = if known { limits.rlim_cur } else { 1024 };
    // "Unlimited" is not a number to loop to; no process here comes near
    // the cap.
    soft.clamp(256, 65_536) as RawFd
}

/// Mark every inherited descriptor below `limit` close-on-exec.
///
/// For the forked child, between fork and exec. It only calls `fcntl`,
/// which is async-signal-safe. Marking and not closing: the standard
/// library reports a failed exec through a pipe of its own in that same
/// range, and that pipe has to work until the exec.
pub(crate) fn seal_inherited(limit: RawFd) {
    for fd in FIRST_INHERITED..limit {
        // SAFETY: plain fcntl; on a number that is not open it fails and
        // changes nothing.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
}

/// Close every descriptor this process inherited, other than the three
/// standard streams. For a process that is going to be a host and nothing
/// else, as the first thing it does, so that it does not hold open for
/// months what its launcher happened to have open at the fork.
///
/// Not for a host run inside another program: it would close that
/// program's files.
pub fn close_inherited_descriptors() {
    // `/dev/fd` lists what is actually open. Collected first: closing
    // while reading would close the directory under the reader.
    let listed: Option<Vec<RawFd>> = std::fs::read_dir("/dev/fd").ok().map(|entries| {
        entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect()
    });
    let descriptors = listed.unwrap_or_else(|| (FIRST_INHERITED..limit()).collect());
    for fd in descriptors {
        if fd >= FIRST_INHERITED {
            // SAFETY: nothing in this process owns a descriptor yet; that
            // is the precondition stated above. Closing a number that is
            // not open is an error with no effect.
            unsafe {
                libc::close(fd);
            }
        }
    }
}
