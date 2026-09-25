//! Sending one agent session's last answer into another session's prompt.
//!
//! The case this is for: one session finishes a piece of work and another has
//! to pick it up - a coordination session asking what an execution session
//! concluded, or a reviewer given what an implementer wrote. Doing that by hand
//! means copying the answer, saving it somewhere, and telling the other agent
//! where it is.
//!
//! **The person sends, and the person submits.** The answer lands on the other
//! agent's input line and is not submitted: the Enter is theirs, after they have
//! seen what arrived and added what they want done with it. Sessions that
//! message each other on their own are what agent CLIs themselves now offer,
//! and the complaint about them is sessions acting on what was said somewhere
//! else. This keeps a person between the two.
//!
//! **Only panes on screen are offered**, the other panes of the project the
//! source is showing in. That is the case the work needs - the two sessions are
//! side by side - and it keeps the list as short as the pane count, the same
//! bound the "Show in" entries have.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::{App, Context, Entity, Window};

use crate::SplitlaneApp;
use crate::claude_sessions::LastAnswer;
use crate::terminal::view::TerminalView;

/// Above this, the answer goes by reference rather than inline. An agent's
/// input line takes a long paste, but the person has to be able to look at what
/// they are about to submit, and a reference is one line they can read.
pub(crate) const INLINE_MAX_BYTES: usize = 16 * 1024;

/// How long a sent-answer file is kept. Long enough for the receiving session to
/// read it whenever it gets to it, short enough that the directory does not
/// become an archive nobody asked for.
const SENT_ANSWER_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A pane that can receive a sent answer.
#[derive(Clone)]
pub(crate) struct AnswerDestination {
    pub(crate) terminal: Entity<TerminalView>,
    /// What the pane is showing, as its header says it.
    pub(crate) label: String,
    /// Where it is (`left`, `top right`, ...), for the hint column.
    pub(crate) slot: &'static str,
}

/// The text to put on the receiving agent's input line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Delivery {
    pub(crate) text: String,
    /// `true` when the answer itself is in `text`, `false` for a reference to
    /// the file that holds it.
    pub(crate) inline: bool,
}

/// What to type into the receiving pane.
///
/// Inline only when the pane has bracketed paste on. Without it the terminal
/// reads each newline in the answer as Enter, and a multi-line answer would be
/// submitted a line at a time - the one thing this must never do. The reference
/// is a single line, so it is safe either way. `None` when the answer cannot go
/// inline and there is no file to point at.
pub(crate) fn compose_delivery(
    source_title: &str,
    answer: &str,
    file: Option<&Path>,
    bracketed_paste: bool,
) -> Option<Delivery> {
    let source = quoted_title(source_title);
    if bracketed_paste && answer.len() <= INLINE_MAX_BYTES {
        // The paste path strips escape sequences already; this does not lean
        // on that. An answer carrying `ESC[201~` would otherwise end the paste
        // early and turn every newline after it into an Enter, and Markdown
        // has no use for either an escape or a C1 control.
        let answer: String = answer
            .chars()
            .filter(|&c| c != '\x1b' && !('\u{0080}'..='\u{009f}').contains(&c))
            .collect();
        return Some(Delivery {
            // The trailing blank line leaves the cursor where the person
            // writes what they want done with it.
            text: format!("The last answer from the session {source}:\n\n{answer}\n\n"),
            inline: true,
        });
    }
    let file = file?;
    let path = file.to_string_lossy();
    // This line may reach a terminal outside bracketed paste, where nothing
    // strips escape sequences on the way in. The title is cleaned above; the
    // path is ours, but it is checked rather than trusted.
    if path.chars().any(char::is_control) {
        return None;
    }
    Some(Delivery {
        text: format!("The last answer from the session {source} is in {path} "),
        inline: false,
    })
}

/// A session title as it can safely appear on another agent's input line:
/// quoted, on one line, with nothing a terminal would act on.
fn quoted_title(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .map(|c| if c == '"' { '\'' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        "\"untitled\"".to_string()
    } else {
        format!("\"{clean}\"")
    }
}

/// The toast for an answer that could not be read, shared with "Copy the last
/// answer" so the two never describe the same failure differently.
pub(crate) fn failure_message(outcome: &LastAnswer) -> Option<&'static str> {
    match outcome {
        LastAnswer::Found(_) => None,
        LastAnswer::NotYet => Some("No answer to copy yet"),
        LastAnswer::NoTranscript => Some("No transcript for this session on disk yet"),
        LastAnswer::Unreadable => Some("Could not read this session's transcript"),
    }
}

/// Where sent-answer files are written: the build's own cache directory, so a
/// from-source build never writes into an installed release's.
fn sent_answers_dir() -> Option<PathBuf> {
    Some(
        dirs::cache_dir()?
            .join(crate::runtime_paths::APP_SUBDIR)
            .join("sent-answers"),
    )
}

/// Write `answer` to a new file in `dir` and drop the ones past their age.
///
/// Every sent answer is written, inline or not: the receiving pane's paste mode is
/// only known on the main thread after this returns, and a file costs nothing
/// next to finding out it was needed and not there.
///
/// Blocking I/O - call from inside `smol::unblock`.
fn write_answer_file_in(
    dir: &Path,
    answer: &str,
    session_id: &str,
    now: SystemTime,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    prune_answer_files(dir, now, SENT_ANSWER_MAX_AGE);
    let stamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // The answer is whatever the agent wrote, which can be anything the session
    // saw. Readable by this user account only, like the transcript it came from.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // Two sends of one session inside a millisecond share a stamp; the second
    // takes a suffix instead of failing, and never replaces the first.
    for attempt in 0..16u32 {
        let name = match attempt {
            0 => format!("{stamp}-{session_id}.md"),
            n => format!("{stamp}-{session_id}-{n}.md"),
        };
        let path = dir.join(name);
        match options.open(&path) {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, answer.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free sent-answer file name",
    ))
}

/// Remove sent-answer files older than `max_age`. Best effort: a file that cannot
/// be removed is left for the next write.
fn prune_answer_files(dir: &Path, now: SystemTime, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if old {
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl SplitlaneApp {
    /// The other panes of the project `source_thread_id` is showing in whose
    /// active surface is a running agent.
    ///
    /// Asked on every frame a menu is up, so it reads only what is in memory.
    /// It cannot tell an agent that exited cleanly from one that is running -
    /// nothing in memory records that - and does not need to: the text goes in
    /// as a paste and is never submitted, so at worst it sits on a shell's input
    /// line where the person can see it.
    pub(crate) fn answer_destinations(
        &self,
        source_thread_id: u64,
        cx: &App,
    ) -> Vec<AnswerDestination> {
        let Some(container) = self.workspaces.iter().find(|ws| {
            ws.root.as_ref().is_some_and(|root| {
                crate::app::agent_slots::agent_pane(root, source_thread_id, cx).is_some()
            })
        }) else {
            return Vec::new();
        };
        let Some(root) = container.root.as_ref() else {
            return Vec::new();
        };
        let form = root.root_form();
        let leaves = root.collect_leaves();
        let count = leaves.len();
        leaves
            .into_iter()
            .enumerate()
            .filter_map(|(idx, pane)| {
                let pane = pane.read(cx);
                let terminal = pane.active_terminal_opt()?.clone();
                let view = terminal.read(cx);
                let thread_id = view.agent_thread_id?;
                if thread_id == source_thread_id || view.terminal.exited.is_some() {
                    return None;
                }
                container
                    .threads
                    .iter()
                    .find(|thread| thread.id == thread_id)?
                    .terminal_agent?;
                Some(AnswerDestination {
                    terminal: terminal.clone(),
                    label: pane.active_tab_label(cx),
                    slot: Self::pane_slot_label(idx, count, form),
                })
            })
            .collect()
    }

    /// Put `source_thread_id`'s last answer on `destination`'s input line.
    ///
    /// The destination is focused at once, so the person is looking at it when
    /// the text arrives; the read and the file write go to a background thread,
    /// and nothing is typed until they come back.
    pub(crate) fn send_last_answer(
        &mut self,
        source_thread_id: u64,
        destination: AnswerDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.thread_by_id(source_thread_id) else {
            return;
        };
        if crate::claude_sessions::transcript_path(thread).is_none() {
            self.show_toast(
                "This session has not written a transcript Splitlane can read",
                cx,
            );
            return;
        }
        let Some(session_id) = thread.session_id.clone() else {
            return;
        };
        // Checked again here because the id becomes part of a filename below.
        if !crate::agent_sessions::is_valid_session_id(&session_id) {
            log::warn!("send last answer: bound session id failed the allow-list");
            return;
        }
        let cwd = thread.cwd.clone();
        let source_title = thread.title.clone();
        let label = destination.label.clone();
        let target = destination.terminal.downgrade();
        self.focus_surface_by_id(destination.terminal.entity_id().as_u64(), window, cx);

        cx.spawn(async move |this, cx| {
            let outcome = smol::unblock(move || {
                match crate::claude_sessions::read_last_answer(&cwd, &session_id) {
                    LastAnswer::Found(answer) => {
                        let file = sent_answers_dir().and_then(|dir| {
                            write_answer_file_in(&dir, &answer, &session_id, SystemTime::now())
                                .map_err(|e| log::warn!("send last answer: cannot write file: {e}"))
                                .ok()
                        });
                        Ok((answer, file))
                    }
                    other => Err(other),
                }
            })
            .await;
            let _ = this.update(cx, |app, cx| {
                let (answer, file) = match outcome {
                    Ok(found) => found,
                    Err(failure) => {
                        if let Some(message) = failure_message(&failure) {
                            app.show_toast(message, cx);
                        }
                        return;
                    }
                };
                let Some(terminal) = target.upgrade() else {
                    app.show_toast(format!("{label} closed before the answer arrived"), cx);
                    return;
                };
                // Asked again now rather than trusted from when the menu was
                // drawn: the read took a moment, and a pane whose process has
                // gone has nobody to read what arrives.
                if terminal.read(cx).terminal.exited.is_some() {
                    app.show_toast(format!("{label} has exited; nothing was sent"), cx);
                    return;
                }
                let bracketed = terminal.read(cx).bracketed_paste_enabled();
                let Some(delivery) = compose_delivery(&source_title, &answer, file.as_deref(), bracketed)
                else {
                    app.show_toast(
                        format!("{label} does not accept a multi-line paste, and the answer could not be saved to a file"),
                        cx,
                    );
                    return;
                };
                terminal.read(cx).type_command(&delivery.text);
                let chars = answer.chars().count();
                if delivery.inline {
                    app.show_toast(
                        format!("Sent the last answer to {label} ({chars} characters), not submitted"),
                        cx,
                    );
                } else {
                    app.show_toast(
                        format!("Sent {label} a reference to the last answer ({chars} characters), not submitted"),
                        cx,
                    );
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_answer_goes_inline_into_a_pane_that_takes_a_paste() {
        let delivery = compose_delivery("Review", "all good", None, true).expect("inline");
        assert!(delivery.inline);
        assert_eq!(
            delivery.text,
            "The last answer from the session \"Review\":\n\nall good\n\n"
        );
    }

    /// Without bracketed paste every newline is an Enter. The answer must not
    /// reach such a pane inline, however short: it would submit line by line.
    #[test]
    fn a_pane_without_bracketed_paste_gets_a_one_line_reference() {
        let file = Path::new("/cache/sent-answers/1-abc.md");
        let delivery =
            compose_delivery("Review", "line one\nline two", Some(file), false).expect("reference");
        assert!(!delivery.inline);
        assert!(!delivery.text.contains(['\n', '\r']));
        assert!(delivery.text.contains("/cache/sent-answers/1-abc.md"));

        assert_eq!(compose_delivery("Review", "x", None, false), None);
        assert_eq!(
            compose_delivery("Review", "x", Some(Path::new("/a\x1b[201~/b.md")), false),
            None
        );
    }

    /// An answer that tries to close the paste early loses the escape, so it
    /// stays text inside the paste.
    #[test]
    fn an_answer_cannot_end_the_paste_early() {
        let delivery =
            compose_delivery("Review", "a\x1b[201~\nrun it\u{9b}", None, true).expect("inline");
        assert!(!delivery.text.contains(['\x1b', '\u{9b}']));
        assert!(delivery.text.contains("a[201~\nrun it"));
    }

    #[test]
    fn two_files_in_one_millisecond_do_not_collide() {
        let dir = std::env::temp_dir().join(format!(
            "splitlane-sent-answer-same-ms-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let now = SystemTime::now();
        let first = write_answer_file_in(&dir, "one", "abc", now).expect("first");
        let second = write_answer_file_in(&dir, "two", "abc", now).expect("second");
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).expect("read"), "one");
        assert_eq!(std::fs::read_to_string(&second).expect("read"), "two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_answer_goes_by_reference() {
        let file = Path::new("/cache/sent-answers/1-abc.md");
        let long = "x".repeat(INLINE_MAX_BYTES + 1);
        let delivery = compose_delivery("Review", &long, Some(file), true).expect("reference");
        assert!(!delivery.inline);
        assert!(!delivery.text.contains(&long));
    }

    /// The title is agent- or user-written and lands on another agent's input
    /// line: it stays on one line, carries no control characters, and cannot
    /// close its own quotes.
    #[test]
    fn the_source_title_is_one_quoted_line() {
        assert_eq!(quoted_title("plain"), "\"plain\"");
        assert_eq!(quoted_title("a\nb\x1b[201~c"), "\"a b [201~c\"");
        assert_eq!(quoted_title("say \"hi\""), "\"say 'hi'\"");
        assert_eq!(quoted_title("  \t "), "\"untitled\"");
    }

    #[test]
    fn an_answer_file_is_written_and_old_ones_are_pruned() {
        let dir =
            std::env::temp_dir().join(format!("splitlane-sent-answer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let stale = dir.join("0-old.md");
        std::fs::write(&stale, "old").expect("stale");
        let keep = dir.join("notes.txt");
        std::fs::write(&keep, "not ours").expect("other");

        // "Now" is far enough ahead that the file written a moment ago is old.
        let later = SystemTime::now() + SENT_ANSWER_MAX_AGE + Duration::from_secs(60);
        let path = write_answer_file_in(&dir, "the answer", "abc-123", later).expect("write");

        assert_eq!(std::fs::read_to_string(&path).expect("read"), "the answer");
        assert!(!stale.exists(), "a file past its age is removed");
        assert!(keep.exists(), "only sent-answer files are pruned");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_failure_has_a_message_and_success_has_none() {
        assert_eq!(failure_message(&LastAnswer::Found("x".into())), None);
        for outcome in [
            LastAnswer::NotYet,
            LastAnswer::NoTranscript,
            LastAnswer::Unreadable,
        ] {
            assert!(failure_message(&outcome).is_some());
        }
    }
}
