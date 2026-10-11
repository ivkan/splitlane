//! A file record of every size a terminal was told to take.
//!
//! A pane was once found drawing its agent in a single column while the pane
//! itself was half a window wide, and nothing on disk said whether the layout
//! had computed one column or a correct size had failed to arrive. A release
//! build logs nowhere, so that question could not be answered after the fact.
//! Sizes change a few times a minute at most, so recording each one costs
//! nothing and the file answers it next time.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Mutex, OnceLock};

const FILE_NAME: &str = "terminal-resize.log";
/// Past this the file is set aside as `.old` and started again, so the record
/// of a long-running install stays bounded at twice this.
const MAX_BYTES: u64 = 256 * 1024;
/// Lines waiting for the file. A disk that has stopped answering loses lines
/// past this; it does not grow the app.
const QUEUE: usize = 1024;

fn sender() -> Option<&'static SyncSender<String>> {
    static SENDER: OnceLock<Option<SyncSender<String>>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            // A test run lays out terminals too, and its sizes are not the
            // record anyone will read.
            if cfg!(test) {
                return None;
            }
            let path = crate::runtime_paths::data_dir()?.join(FILE_NAME);
            let (tx, rx) = sync_channel::<String>(QUEUE);
            // The callers are on the render thread; the file is not.
            std::thread::Builder::new()
                .name("terminal-resize-journal".into())
                .spawn(move || {
                    for message in rx {
                        // Stamped here: reading the local time can touch the
                        // time zone file, which the render thread must not.
                        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                        append(&path, &format!("{stamp} {message}\n"));
                    }
                })
                .ok()?;
            Some(tx)
        })
        .as_ref()
}

fn append(path: &PathBuf, line: &str) {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let _ = options
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

/// Record one line; the writer stamps it with the local time.
pub(crate) fn record(message: &str) {
    let Some(sender) = sender() else {
        return;
    };
    let _ = sender.try_send(message.to_string());
}

/// Record `message` unless it is what `key` said last time. For a condition
/// that holds across many frames and is worth one line, not one per frame.
pub(crate) fn record_change(key: u64, message: &str) {
    static LAST: OnceLock<Mutex<HashMap<u64, String>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if last.get(&key).is_some_and(|previous| previous == message) {
        return;
    }
    last.insert(key, message.to_string());
    drop(last);
    record(message);
}
