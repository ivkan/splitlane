//! A file the app appends lines to from a thread of its own, kept under a
//! size by moving it aside once.
//!
//! Callers sit on the thread that draws the window, and a write there waits
//! on the disk: a slow or sleeping drive would freeze every pane for as long
//! as it took. So the caller only hands the bytes over, and a line that
//! cannot be written is dropped rather than retried - these files are for
//! reading after the fact, and none of what they hold is worth a stall.
//!
//! The writer never logs. The app's own log is one of its files, and a
//! complaint about writing it would come back here.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// How many lines may wait for the writer. A disk that does not answer
/// would otherwise have the queue grow for as long as it slept, in the
/// process that draws the window; past this, lines are dropped.
const QUEUE_LINES: usize = 4096;

pub(crate) struct Appender(mpsc::SyncSender<Vec<u8>>);

impl Appender {
    /// Start the writer for `path`. Once the file would grow past
    /// `max_bytes` it becomes `<name>.1`, replacing the one before, and a new
    /// file is begun: two files at most, and the newest lines always kept.
    pub(crate) fn start(path: PathBuf, max_bytes: u64) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE_LINES);
        std::thread::Builder::new()
            .name(format!(
                "log-file:{}",
                path.file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default()
            ))
            .spawn(move || {
                let mut file = RotatingFile::new(path, max_bytes);
                for bytes in rx {
                    file.append(&bytes);
                }
            })?;
        Ok(Self(tx))
    }

    pub(crate) fn append(&self, bytes: Vec<u8>) {
        let _ = self.0.try_send(bytes);
    }
}

struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    open: Option<(std::fs::File, u64)>,
    /// A write failed part of the way through a line, which may be in the
    /// file without its newline. The next line starts on a line of its own,
    /// so one reader's parse error does not take the line after it too.
    torn: bool,
}

impl RotatingFile {
    fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            open: None,
            torn: false,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        if self.open.is_none() {
            self.open = open_append(&self.path);
        }
        // An empty file takes the line whatever its size, or a line longer
        // than the cap would rotate on every write.
        if self
            .open
            .as_ref()
            .is_some_and(|(_, len)| *len > 0 && len + bytes.len() as u64 > self.max_bytes)
        {
            self.open = None;
            let _ = std::fs::rename(&self.path, rotated(&self.path));
            self.open = open_append(&self.path);
        }
        if let Some((file, len)) = self.open.as_mut() {
            if self.torn && *len > 0 && file.write_all(b"\n").is_ok() {
                *len += 1;
            }
            self.torn = false;
            if file.write_all(bytes).is_ok() {
                *len += bytes.len() as u64;
            } else {
                self.torn = true;
                // Opened again on the next line: the file may have been
                // removed, or the disk filled and was cleared.
                self.open = None;
            }
        }
    }
}

/// `app.log` -> `app.log.1`.
pub(crate) fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

fn open_append(path: &Path) -> Option<(std::fs::File, u64)> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Paths, session ids and working hours: the user's own business.
        options.mode(0o600);
    }
    let file = options.open(path).ok()?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    Some((file, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_past_its_cap_is_moved_aside_once_and_begun_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("app.log");
        let mut file = RotatingFile::new(path.clone(), 10);
        file.append(b"first\n");
        file.append(b"second\n");
        file.append(b"third\n");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "third\n");
        assert_eq!(
            std::fs::read_to_string(rotated(&path)).expect("read rotated"),
            "second\n",
            "the older file is replaced, not kept"
        );
    }

    #[test]
    fn a_line_after_a_torn_one_starts_on_a_line_of_its_own() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.log");
        let mut file = RotatingFile::new(path.clone(), 1024);
        file.append(b"{\"whole\":1}\n");
        // As a write that stopped part of the way through leaves it.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(b"{\"to"))
            .expect("tear");
        file.open = None;
        file.torn = true;
        file.append(b"{\"next\":2}\n");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "{\"whole\":1}\n{\"to\n{\"next\":2}\n"
        );
    }

    #[test]
    fn a_line_longer_than_the_cap_is_still_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.log");
        let mut file = RotatingFile::new(path.clone(), 4);
        file.append(b"a long line\n");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "a long line\n"
        );
        assert!(!rotated(&path).exists());
    }

    #[test]
    fn an_existing_file_is_appended_to_and_its_size_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.log");
        std::fs::write(&path, b"0123456789").expect("seed");
        let mut file = RotatingFile::new(path.clone(), 12);
        file.append(b"abc\n");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "abc\n");
        assert_eq!(
            std::fs::read_to_string(rotated(&path)).expect("read rotated"),
            "0123456789"
        );
    }

    #[test]
    fn lines_handed_to_the_writer_arrive_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.log");
        let appender = Appender::start(path.clone(), 1024).expect("start");
        for n in 0..50 {
            appender.append(format!("{n}\n").into_bytes());
        }
        drop(appender);
        let expected: String = (0..50).map(|n| format!("{n}\n")).collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let got = std::fs::read_to_string(&path).unwrap_or_default();
            if got == expected {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "got {got:?}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
