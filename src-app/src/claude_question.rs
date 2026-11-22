//! The question a waiting Claude Code session is asking, read from the end of
//! its own transcript.
//!
//! The status file says only that the session waits, and at best in two words
//! (`permission prompt`, `input needed`). A lead that opened the session and is
//! told it is waiting needs the question itself to pass it on, and without it
//! every such wait was followed by a `status` and a `read` of the pane. The
//! call the agent is waiting on is already in its transcript: Claude Code
//! writes the `tool_use` before it asks whether it may run it, and a question
//! put with `AskUserQuestion` is such a call too. So the newest call nothing
//! has answered yet is the question.
//!
//! The text is the agent's own and is handled as untrusted: it is kept to one
//! bounded line and shown as data, never interpreted.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::limits::MAX_LINE_BYTES;

/// How much of the transcript's end is read. The call being asked about is the
/// newest record, so a short tail holds it; the bound keeps a read that runs
/// every state pass while a session waits cheap on a large file.
const TAIL_BYTES: u64 = 128 * 1024;

/// The most of a question that is kept, in characters.
pub(crate) const QUESTION_MAX_CHARS: usize = 400;

/// Calls that are open for as long as a subagent works. They are not what a
/// person is asked about, and one can stay open beside the call that is.
const LONG_RUNNING_CALLS: [&str; 2] = ["Task", "Agent"];

/// The question the session at `path` is waiting on, or `None` when its
/// transcript shows no call left unanswered. Blocking I/O.
pub(crate) fn pending_question(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file);
    if start > 0 {
        // The window opens mid-line; the fragment is not a record.
        let mut partial = Vec::new();
        let _ = reader.read_until(b'\n', &mut partial);
    }
    pending_question_in(reader)
}

fn pending_question_in(mut reader: impl BufRead) -> Option<String> {
    // Calls started and not answered, oldest first: (id, name, input).
    let mut open: Vec<(String, String, Value)> = Vec::new();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let read = reader
            .by_ref()
            .take(MAX_LINE_BYTES)
            .read_until(b'\n', &mut raw)
            .ok()?;
        if read == 0 {
            break;
        }
        if raw.last() != Some(&b'\n') {
            if read as u64 == MAX_LINE_BYTES {
                // An oversize record: drop the rest of it, never parse a part.
                let mut rest = Vec::new();
                let _ = reader.read_until(b'\n', &mut rest);
            }
            // A short unterminated line is a record still being written.
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        let Some(content) = value
            .get("message")
            .and_then(|message| message.get("content"))
        else {
            continue;
        };
        // A person's prompt answers everything before it.
        if value.get("type").and_then(Value::as_str) == Some("user")
            && content.is_string()
            && value.get("isSidechain").and_then(Value::as_bool) != Some(true)
        {
            open.clear();
            continue;
        }
        let Some(blocks) = content.as_array() else {
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (
                        block.get("id").and_then(Value::as_str),
                        block.get("name").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    open.push((id.to_string(), name.to_string(), input));
                }
                Some("tool_result") => {
                    if let Some(id) = block.get("tool_use_id").and_then(Value::as_str) {
                        open.retain(|(open_id, ..)| open_id != id);
                    }
                }
                _ => {}
            }
        }
    }
    open.iter()
        .rev()
        .find(|(_, name, _)| !LONG_RUNNING_CALLS.contains(&name.as_str()))
        .map(|(_, name, input)| describe(name, input))
}

/// One line saying what the call asks of a person.
fn describe(name: &str, input: &Value) -> String {
    let text = |key: &str| input.get(key).and_then(Value::as_str).map(str::trim);
    let line = match name {
        "AskUserQuestion" => {
            let questions: Vec<String> = input
                .get("questions")
                .and_then(Value::as_array)
                .map(|questions| {
                    questions
                        .iter()
                        .filter_map(|question| {
                            let asked = question.get("question").and_then(Value::as_str)?;
                            let options: Vec<&str> = question
                                .get("options")
                                .and_then(Value::as_array)
                                .map(|options| {
                                    options
                                        .iter()
                                        .filter_map(|option| {
                                            option.get("label").and_then(Value::as_str)
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            Some(if options.is_empty() {
                                asked.to_string()
                            } else {
                                format!("{asked} (options: {})", options.join(" / "))
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if questions.is_empty() {
                "Question to a person".to_string()
            } else {
                format!("Question: {}", questions.join("; "))
            }
        }
        "ExitPlanMode" => match text("plan") {
            Some(plan) if !plan.is_empty() => format!("Approve the plan: {plan}"),
            _ => "Approve the plan".to_string(),
        },
        _ => {
            let detail = [
                "command",
                "file_path",
                "notebook_path",
                "url",
                "pattern",
                "path",
                "query",
                "description",
            ]
            .iter()
            .find_map(|key| text(key).filter(|value| !value.is_empty()));
            match detail {
                Some(detail) => format!("Permission to use {name}: {detail}"),
                None => format!("Permission to use {name}"),
            }
        }
    };
    one_line(&line, QUESTION_MAX_CHARS)
}

/// Whitespace runs folded to one space, and the text cut to `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() <= max {
        return folded;
    }
    let mut cut: String = folded.chars().take(max.saturating_sub(3)).collect();
    cut.push_str("...");
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(lines: &[&str]) -> Option<String> {
        let mut text = lines.join("\n");
        text.push('\n');
        pending_question_in(std::io::Cursor::new(text.into_bytes()))
    }

    const PROMPT: &str = r#"{"type":"user","message":{"role":"user","content":"run the build"}}"#;

    #[test]
    fn a_permission_ask_names_the_tool_and_what_it_would_run() {
        let asked = question(&[
            PROMPT,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Cleaning up."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"rm -rf build\n  && make","description":"Clean"}}]}}"#,
        ]);
        assert_eq!(
            asked.as_deref(),
            Some("Permission to use Bash: rm -rf build && make")
        );
    }

    #[test]
    fn a_question_put_with_ask_user_question_is_given_with_its_options() {
        let asked = question(&[
            PROMPT,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"questions":[{"question":"Which database?","header":"DB","options":[{"label":"Postgres","description":"x"},{"label":"SQLite","description":"y"}],"multiSelect":false}]}}]}}"#,
        ]);
        assert_eq!(
            asked.as_deref(),
            Some("Question: Which database? (options: Postgres / SQLite)")
        );
    }

    #[test]
    fn answered_calls_and_calls_before_a_prompt_are_not_questions() {
        assert_eq!(
            question(&[
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            ]),
            None
        );
        assert_eq!(
            question(&[
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
                PROMPT,
            ]),
            None
        );
    }

    /// A subagent left running is not what the person is asked about; the
    /// call beside it is.
    #[test]
    fn a_subagent_in_flight_is_passed_over() {
        let asked = question(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/src/main.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a1","name":"Task","input":{"description":"explore"}}]}}"#,
        ]);
        assert_eq!(
            asked.as_deref(),
            Some("Permission to use Edit: /src/main.rs")
        );
        assert_eq!(
            question(&[
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a1","name":"Task","input":{"description":"explore"}}]}}"#
            ]),
            None
        );
    }

    #[test]
    fn a_long_question_is_cut_to_one_bounded_line() {
        let long = "x".repeat(QUESTION_MAX_CHARS * 2);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"{long}"}}}}]}}}}"#
        );
        let asked = question(&[&line]).expect("a question");
        assert_eq!(asked.chars().count(), QUESTION_MAX_CHARS);
        assert!(asked.ends_with("..."));
    }

    /// A record still being written is not read, and neither is part of one.
    #[test]
    fn an_unterminated_record_is_not_read() {
        let text = format!(
            "{PROMPT}\n{}",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#
        );
        assert_eq!(
            pending_question_in(std::io::Cursor::new(text.into_bytes())),
            None
        );
    }
}
