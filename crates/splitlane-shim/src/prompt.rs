//! The first prompt of a session the app opened, handed over as an argument.
//!
//! The app leaves the text in a file of its own and names the file in the
//! launch command by a key made of hex digits, so no part of the text is ever
//! read by the shell the command is typed into. The shim swaps the key for
//! the text. The agent then receives the prompt the way it receives one typed
//! after its name on a command line, and not as a paste.
//!
//! Removing the file is the receipt: the app reads its absence as "the agent
//! was started with the prompt".

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant};

/// The launch argument carrying the key. Never passed on to the agent.
pub(crate) const OPENING_PROMPT_FLAG: &str = "--splitlane-opening-prompt=";
/// The directory the app keeps such files in, exported to every pane.
pub(crate) const PROMPT_DIR_ENV: &str = "SPLITLANE_PROMPT_DIR";

/// A key is exactly what the app mints: 32 lowercase hex digits. Anything
/// else could name a file outside the directory.
fn is_key(key: &str) -> bool {
    key.len() == 32
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// How long the shim waits for a file its key names. The app writes it off
/// its main thread while the launch command is already on its way to the
/// shell, so on a busy machine the command can get here first.
pub(crate) const PATIENCE: Duration = Duration::from_millis(500);
const PATIENCE_STEP: Duration = Duration::from_millis(20);

/// `args` with the key argument removed and, when the file it names could be
/// read, the prompt appended as the last argument.
pub(crate) fn with_opening_prompt(
    args: Vec<OsString>,
    dir: Option<&Path>,
    patience: Duration,
) -> Vec<OsString> {
    let mut key = None;
    let mut out = Vec::with_capacity(args.len() + 1);
    for arg in args {
        match arg
            .to_str()
            .and_then(|a| a.strip_prefix(OPENING_PROMPT_FLAG))
        {
            Some(found) => key = Some(found.to_string()),
            None => out.push(arg),
        }
    }
    let Some(key) = key else {
        return out;
    };
    let Some(dir) = dir else {
        crate::diagnose("opening prompt: a key was given and no directory to look in");
        return out;
    };
    if !is_key(&key) {
        crate::diagnose("opening prompt: the key is not one this app mints; ignored");
        return out;
    }
    let path = dir.join(&key);
    let started = Instant::now();
    let read = loop {
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == ErrorKind::NotFound && started.elapsed() < patience => {
                std::thread::sleep(PATIENCE_STEP);
            }
            read => break read,
        }
    };
    match read {
        Ok(text) => {
            // Removed only once read: a file left behind tells the app the
            // prompt did not reach the agent.
            if let Err(e) = std::fs::remove_file(&path) {
                crate::diagnose(&format!("opening prompt: read, but not removed: {e}"));
            }
            if !text.is_empty() {
                out.push(OsString::from(text));
            }
        }
        Err(e) => crate::diagnose(&format!("opening prompt: not read: {e}")),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_key_is_swapped_for_the_text_and_the_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(KEY);
        std::fs::write(&file, "fix it\n\n'quoted' $HOME `x`").unwrap();
        let flag = format!("{OPENING_PROMPT_FLAG}{KEY}");
        let out = with_opening_prompt(
            args(&["--session-id", "abc", &flag]),
            Some(dir.path()),
            Duration::ZERO,
        );
        assert_eq!(
            out,
            args(&["--session-id", "abc", "fix it\n\n'quoted' $HOME `x`"])
        );
        assert!(!file.exists(), "the missing file is the app's receipt");
    }

    /// The agent must never see an argument it does not know.
    #[test]
    fn the_key_argument_never_reaches_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let flag = format!("{OPENING_PROMPT_FLAG}{KEY}");
        // No file by that key.
        assert_eq!(
            with_opening_prompt(args(&["-c", &flag]), Some(dir.path()), Duration::ZERO),
            args(&["-c"])
        );
        // No directory to look in.
        assert_eq!(
            with_opening_prompt(args(&[&flag]), None, Duration::ZERO),
            args(&[])
        );
    }

    #[test]
    fn a_key_that_is_not_one_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("secret");
        std::fs::write(&outside, "not a prompt").unwrap();
        let inner = dir.path().join("prompts");
        std::fs::create_dir(&inner).unwrap();
        let flag = format!("{OPENING_PROMPT_FLAG}../secret");
        assert_eq!(
            with_opening_prompt(args(&[&flag]), Some(&inner), Duration::ZERO),
            args(&[])
        );
        assert!(outside.exists());
    }

    #[test]
    fn arguments_without_a_key_pass_untouched() {
        let given = args(&["--resume", "abc", "a prompt typed by hand"]);
        assert_eq!(
            with_opening_prompt(given.clone(), None, Duration::ZERO),
            given
        );
    }

    /// The app's write and the shell's start race; the shim gives the file
    /// a moment rather than start the agent without its task.
    #[test]
    fn a_file_that_arrives_a_moment_late_is_still_collected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(KEY);
        let late = file.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            // Whole or absent, as the app leaves it: written beside, then
            // moved into place.
            let tmp = late.with_extension("tmp");
            std::fs::write(&tmp, "late").unwrap();
            std::fs::rename(&tmp, &late).unwrap();
        });
        let flag = format!("{OPENING_PROMPT_FLAG}{KEY}");
        let out = with_opening_prompt(args(&[&flag]), Some(dir.path()), Duration::from_secs(5));
        writer.join().unwrap();
        assert_eq!(out, args(&["late"]));
    }
}
