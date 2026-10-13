//! Terminal state and view - PTY management and GPUI view wrapper.
//!
//! Manages the alacritty_terminal `Term`, its `tty` + `EventLoop`, and
//! periodic sync. The TerminalView creates a TerminalElement for cell-by-cell
//! rendering.

#[cfg(test)]
pub(crate) mod backend_corpus;
pub mod blink;
pub mod element;
#[cfg(any(
    all(target_os = "linux", feature = "libghostty-linux"),
    all(
        target_os = "windows",
        target_arch = "x86_64",
        target_env = "msvc",
        feature = "libghostty-windows"
    )
))]
mod ghostty_session;
#[cfg(all(
    test,
    any(
        all(target_os = "linux", feature = "libghostty-linux"),
        all(
            target_os = "windows",
            target_arch = "x86_64",
            target_env = "msvc",
            feature = "libghostty-windows"
        )
    )
))]
mod ghostty_stress;
// A pane can run in the session host on macOS only so far: it is the one
// platform where the host has the emulator the window uses.
pub(crate) mod held_sessions;
#[cfg(target_os = "macos")]
mod host_session;
mod input;
mod listener;
mod marks;
#[cfg(all(test, target_os = "linux"))]
mod portable_pty_probe;
pub(crate) mod program_status;
mod pty_session;
mod resize_journal;
mod search;
mod service_detector;
// Plain bookkeeping, built and tested everywhere. Outside its tests it is
// used only where a pane can run in the session host.
pub mod shell;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod stream_replay;
pub mod types;
pub mod view;

pub use listener::{SpikeTermSize, ZedListener};
pub(crate) use pty_session::TerminalSessionBackend;
pub use pty_session::TerminalState;
#[cfg(test)]
pub(crate) use pty_session::{
    start_render_content_timing_probe, take_render_content_lock_durations,
};
pub use service_detector::ServiceInfo;
pub use view::{TerminalEvent, TerminalView};

#[cfg(debug_assertions)]
pub(crate) use view::probe_enabled;
