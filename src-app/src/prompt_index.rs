//! What a person wrote in each session, held in memory so the palette can
//! search it as they type.
//!
//! A session's title is written once, near its start, so a long session that
//! moved through several subjects is findable by none of the later ones. The
//! words the person typed are the record of what it was about, and they are a
//! small part of a transcript: measured on one machine, 96 Claude Code
//! transcripts of one project held 479 MB, of which the prompts were under
//! 1 MB. So the prompts are read once, off the render thread, and every query
//! after that is a scan of memory.
//!
//! **Prompts, not answers.** The agent's answers and tool output are most of
//! the bytes and most of the false hits: a word the person never used turns
//! up in a file the agent read. What someone remembers discussing is what they
//! said.
//!
//! **Appended, not re-read.** A transcript only grows, and the one in use grows
//! every turn. The cache keeps how far each file was read, to the end of its
//! last whole line, and the next read starts there. A file that got shorter
//! was replaced, and is read again from the start.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::agent_sessions::SessionAgent;

/// The longest record read. A pasted call transcript is one record and runs to
/// a few hundred KB; a tool result can be tens of MB and is never a prompt, so
/// anything past this is skipped without being held.
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

/// How much of the text before a match a snippet keeps, in characters. The
/// rest of the row's width goes to what follows the match, which is what says
/// what the sentence was about.
const SNIPPET_LEAD_CHARS: usize = 32;

/// One session's prompts, whitespace folded so a match can span a line break,
/// each beside its lowercase form.
#[derive(Debug, Default)]
pub(crate) struct SessionPrompts {
    prompts: Vec<Prompt>,
}

#[derive(Debug, Clone)]
struct Prompt {
    text: String,
    lower: String,
}

/// Where a query matched in one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptHit {
    /// Text before the match, cut to [`SNIPPET_LEAD_CHARS`], with a leading
    /// ellipsis when cut.
    pub(crate) before: String,
    pub(crate) matched: String,
    pub(crate) after: String,
    /// How many of the session's prompts match, the shown one included.
    pub(crate) prompts: usize,
}

impl SessionPrompts {
    fn push(&mut self, raw: &str) {
        let text = fold_whitespace(raw);
        if text.is_empty() {
            return;
        }
        // Codex writes one turn in two record shapes a few lines apart.
        if self.prompts.last().is_some_and(|last| last.text == text) {
            return;
        }
        let lower = text.to_lowercase();
        self.prompts.push(Prompt { text, lower });
    }

    /// The newest prompt that contains `needle`, which must already be
    /// lowercase with its whitespace folded. The newest, because the list is
    /// sorted by last activity and a session is opened to carry on from where
    /// it got to.
    pub(crate) fn find(&self, needle: &str) -> Option<PromptHit> {
        if needle.is_empty() {
            return None;
        }
        let mut matching = self
            .prompts
            .iter()
            .rev()
            .filter(|prompt| prompt.lower.contains(needle));
        let newest = matching.next()?;
        Some(snippet(newest, needle, 1 + matching.count()))
    }
}

fn snippet(prompt: &Prompt, needle: &str, prompts: usize) -> PromptHit {
    // Lowercasing keeps byte lengths for ASCII and Cyrillic, which is nearly
    // every prompt; where it did not, the snippet is cut from the lowercase
    // form rather than at an offset that means something else in the original.
    let source = if prompt.lower.len() == prompt.text.len() {
        &prompt.text
    } else {
        &prompt.lower
    };
    let start = prompt.lower.find(needle).unwrap_or(0);
    let end = start + needle.len();
    let (Some(head), Some(matched), Some(after)) = (
        source.get(..start),
        source.get(start..end),
        source.get(end..),
    ) else {
        return PromptHit {
            before: String::new(),
            matched: prompt.lower.get(start..end).unwrap_or_default().to_string(),
            after: String::new(),
            prompts,
        };
    };
    let lead: Vec<char> = head.chars().collect();
    let before = if lead.len() > SNIPPET_LEAD_CHARS {
        let kept: String = lead[lead.len() - SNIPPET_LEAD_CHARS..].iter().collect();
        format!("\u{2026}{}", kept.trim_start())
    } else {
        head.to_string()
    };
    PromptHit {
        before,
        matched: matched.to_string(),
        after: after.to_string(),
        prompts,
    }
}

/// A query in the form prompts are held in.
pub(crate) fn normalize_query(query: &str) -> String {
    fold_whitespace(query).to_lowercase()
}

fn fold_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Entry {
    /// Where the next read starts: just past the last whole line read.
    read_to: u64,
    prompts: Arc<SessionPrompts>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The prompts in `path`, read from where the last call stopped.
///
/// Blocking I/O - call from inside `smol::unblock`.
pub(crate) fn prompts_of(agent: SessionAgent, path: &Path) -> Arc<SessionPrompts> {
    let extract: fn(&serde_json::Value) -> Option<String> = match agent {
        SessionAgent::Claude => crate::claude_sessions::prompt_text,
        SessionAgent::Codex => crate::codex_sessions::prompt_text,
        _ => return Arc::default(),
    };
    let Ok(len) = std::fs::metadata(path).map(|meta| meta.len()) else {
        return Arc::default();
    };
    let previous = cache().lock().ok().and_then(|cache| {
        cache
            .get(path)
            .map(|entry| (entry.read_to, entry.prompts.clone()))
    });
    let (start, mut session) = match previous {
        Some((read_to, prompts)) if read_to == len => return prompts,
        Some((read_to, prompts)) if read_to < len => (
            read_to,
            SessionPrompts {
                prompts: prompts.prompts.clone(),
            },
        ),
        _ => (0, SessionPrompts::default()),
    };
    let read_to = read_prompts(path, start, extract, &mut session).unwrap_or(start);
    let session = Arc::new(session);
    if let Ok(mut cache) = cache().lock() {
        cache.insert(
            path.to_path_buf(),
            Entry {
                read_to,
                prompts: session.clone(),
            },
        );
    }
    session
}

/// Read whole lines from `start`, handing each record that can hold a prompt
/// to `extract`. Returns the offset just past the last whole line, so a line
/// still being written is read in full next time.
fn read_prompts(
    path: &Path,
    start: u64,
    extract: fn(&serde_json::Value) -> Option<String>,
    session: &mut SessionPrompts,
) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(file);
    let mut offset = start;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut line)
            .ok()?;
        if read == 0 {
            return Some(offset);
        }
        if line.last() != Some(&b'\n') {
            if (read as u64) <= MAX_RECORD_BYTES {
                // The end of the file, mid-line: the agent is still writing it.
                return Some(offset);
            }
            // Longer than any prompt: skip to the end of it without holding it.
            let mut skipped = read as u64;
            loop {
                let chunk = reader.fill_buf().ok()?;
                if chunk.is_empty() {
                    return Some(offset);
                }
                if let Some(newline) = chunk.iter().position(|&b| b == b'\n') {
                    reader.consume(newline + 1);
                    skipped += newline as u64 + 1;
                    break;
                }
                let len = chunk.len();
                reader.consume(len);
                skipped += len as u64;
            }
            offset += skipped;
            continue;
        }
        offset += read as u64;
        if !may_hold_prompt(&line) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(text) = extract(&value) {
            session.push(&text);
        }
    }
}

/// A cheap gate before parsing: most lines of a transcript are the agent's
/// own, and both stores name a person's record in a way these catch.
fn may_hold_prompt(line: &[u8]) -> bool {
    const MARKS: [&[u8]; 3] = [b"\"user\"", b"UserMessage", b"user_message"];
    MARKS
        .iter()
        .any(|mark| line.windows(mark.len()).any(|window| window == *mark))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn claude_prompt(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": text}
        })
        .to_string()
    }

    fn claude_tool_result(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "toolUseResult": {},
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": text}
            ]}
        })
        .to_string()
    }

    fn write(dir: &tempfile::TempDir, name: &str, lines: &[String]) -> PathBuf {
        let path = dir.path().join(name);
        let mut file = File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn a_session_is_found_by_a_later_prompt_not_only_its_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "a.jsonl",
            &[
                claude_prompt("set up the deploy script"),
                claude_prompt("now about the Stripe webhook retries"),
            ],
        );
        let prompts = prompts_of(SessionAgent::Claude, &path);
        let hit = prompts.find(&normalize_query("stripe")).unwrap();
        assert_eq!(hit.matched, "Stripe");
        assert_eq!(hit.after, " webhook retries");
        assert_eq!(hit.prompts, 1);
    }

    /// A word the agent read in a file is not something the person discussed.
    #[test]
    fn what_a_tool_returned_is_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "b.jsonl",
            &[
                claude_prompt("look at the config"),
                claude_tool_result("stripe_key = sk_live_..."),
            ],
        );
        let prompts = prompts_of(SessionAgent::Claude, &path);
        assert_eq!(prompts.find("stripe"), None);
    }

    #[test]
    fn a_match_spans_a_line_break_and_ignores_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "c.jsonl",
            &[claude_prompt("Café:\nÉtude   über Migration")],
        );
        let prompts = prompts_of(SessionAgent::Claude, &path);
        let hit = prompts.find(&normalize_query("ÉTUDE ÜBER")).unwrap();
        assert_eq!(hit.before, "Café: ");
        assert_eq!(hit.matched, "Étude über");
    }

    #[test]
    fn the_newest_mention_is_shown_and_the_others_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "d.jsonl",
            &[
                claude_prompt("first about pricing"),
                claude_prompt("unrelated"),
                claude_prompt("pricing again, the annual plan"),
            ],
        );
        let hit = prompts_of(SessionAgent::Claude, &path)
            .find("pricing")
            .unwrap();
        assert_eq!(hit.after, " again, the annual plan");
        assert_eq!(hit.prompts, 2);
    }

    #[test]
    fn a_long_lead_is_cut_from_the_front() {
        let dir = tempfile::tempdir().unwrap();
        let lead = "word ".repeat(30);
        let path = write(
            &dir,
            "e.jsonl",
            &[claude_prompt(&format!("{lead}target here"))],
        );
        let hit = prompts_of(SessionAgent::Claude, &path)
            .find("target")
            .unwrap();
        assert!(hit.before.starts_with('\u{2026}'));
        assert!(hit.before.chars().count() <= SNIPPET_LEAD_CHARS + 1);
    }

    /// The session in use grows every turn; the next read picks up the new
    /// prompts, and a line still being written is not read as half a record.
    #[test]
    fn a_growing_transcript_is_read_on_from_where_it_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "f.jsonl", &[claude_prompt("first topic")]);
        assert!(
            prompts_of(SessionAgent::Claude, &path)
                .find("second")
                .is_none()
        );

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let record = claude_prompt("second topic");
        let (head, tail) = record.split_at(10);
        write!(file, "{head}").unwrap();
        file.flush().unwrap();
        assert!(
            prompts_of(SessionAgent::Claude, &path)
                .find("second")
                .is_none()
        );

        writeln!(file, "{tail}").unwrap();
        file.flush().unwrap();
        let prompts = prompts_of(SessionAgent::Claude, &path);
        assert!(prompts.find("second").is_some());
        assert!(prompts.find("first").is_some());
    }

    #[test]
    fn a_record_too_long_for_a_prompt_is_skipped_and_reading_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let huge = claude_tool_result(&"x".repeat(MAX_RECORD_BYTES as usize + 10));
        let path = write(&dir, "g.jsonl", &[huge, claude_prompt("after the big one")]);
        assert!(
            prompts_of(SessionAgent::Claude, &path)
                .find("big one")
                .is_some()
        );
    }

    #[test]
    fn codex_writes_a_turn_twice_and_it_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        let response_item = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "<environment_context>cwd</environment_context>"},
                {"type": "input_text", "text": "rename the billing module"}
            ]}
        })
        .to_string();
        let item_completed = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "item_completed", "item": {"type": "UserMessage", "content": [
                {"type": "text", "text": "rename the billing module"}
            ]}}
        })
        .to_string();
        let path = write(&dir, "h.jsonl", &[response_item, item_completed]);
        let prompts = prompts_of(SessionAgent::Codex, &path);
        let hit = prompts.find("billing").unwrap();
        assert_eq!(hit.prompts, 1);
        assert!(prompts.find("environment_context").is_none());
    }
}
