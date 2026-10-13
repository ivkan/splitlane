//! The session host process: this binary started as
//! `splitlane __splitlane-host`.
//!
//! The host itself is `splitlane-host`, a crate that knows nothing of the
//! app. What is decided here is what only the app knows: where this build's
//! host listens, which binary watches over a session's children, and where
//! a process with no standard streams can leave a log.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use crate::runtime_paths;

/// A log this size is cut back to nothing when the next host starts. A host
/// writes a few lines per session, so this is months of history.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Run as the session host until it is told to stop or has been idle long
/// enough. Returns the process exit code.
pub fn run() -> i32 {
    // Before anything is opened: whatever the launcher had open at the fork
    // would otherwise stay open for as long as the host lives.
    splitlane_host::close_inherited_descriptors();
    install_file_logger();
    // After the logger, so a refusal is on record. A host is allowed 256
    // open files when started from a desktop session, and each pane in it
    // costs eight.
    splitlane_host::raise_descriptor_limit();
    let Some(socket) = runtime_paths::host_socket_path_spec() else {
        return fail("cannot work out where the session host socket belongs");
    };
    let mut config = splitlane_host::HostConfig::new(socket.path());
    config.owns_socket_dir = socket.owned_parent();
    // The watcher that ends a session's processes if the host is killed is
    // this same binary, as it is for the PTYs the app owns itself.
    config.guard_exe = std::env::current_exe().ok();
    config.handle_signals = true;
    config.version = env!("CARGO_PKG_VERSION").to_string();
    match splitlane_host::run(config) {
        Ok(()) => 0,
        // Two launchers raced and the other one's host has the socket. That
        // is the state both wanted.
        Err(splitlane_host::HostError::AlreadyRunning(path)) => {
            log::info!("a session host already runs on {}", path.display());
            0
        }
        Err(e) => fail(&e.to_string()),
    }
}

fn fail(message: &str) -> i32 {
    log::error!("session host: {message}");
    // For whoever started it by hand; a launcher has closed this stream.
    eprintln!("splitlane: session host: {message}");
    1
}

struct FileLogger(Mutex<File>);

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut file = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(
            file,
            "{} {:5} {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            record.level(),
            record.args()
        );
    }

    fn flush(&self) {
        let _ = self.0.lock().unwrap_or_else(|e| e.into_inner()).flush();
    }
}

/// Send the `log` macros to the host's log file. Without a writable file
/// the host runs unlogged: a session is worth more than a line about it.
fn install_file_logger() {
    let Some(file) = runtime_paths::host_log_path().and_then(|path| open_log(&path)) else {
        return;
    };
    if log::set_boxed_logger(Box::new(FileLogger(Mutex::new(file)))).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

fn open_log(path: &Path) -> Option<File> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    let oversized = std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES);
    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    if oversized {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Session ids, pids and working habits: the user's own business.
        options.mode(0o600);
    }
    options.open(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_is_appended_to_until_it_is_too_large_and_then_started_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("host.log");

        writeln!(open_log(&path).expect("created"), "first").expect("write");
        writeln!(open_log(&path).expect("reopened"), "second").expect("write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "first\nsecond\n"
        );

        std::fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).expect("fill");
        writeln!(open_log(&path).expect("truncated"), "fresh").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "fresh\n");
    }
}
