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

/// How much of what follows a match a snippet keeps. A row is one line wide,
/// and the rest of a pasted call transcript is a few hundred KB the render
/// pass would otherwise lay out for every matching row.
const SNIPPET_TAIL_CHARS: usize = 160;

/// The longest prompt kept, in bytes. A pasted call transcript is the longest
/// thing a person sends and stays under it; a larger paste is a log, kept up
/// to here, so a few pastes cannot grow the cache past what was measured.
const MAX_PROMPT_BYTES: usize = 512 * 1024;

/// One session's prompts, whitespace folded so a match can span a line break,
/// each beside its lowercase form. The lowercase form has exactly one
/// character for each of the text's, so a match found in it is at the same
/// character position in the text - a byte offset is not, since lowercasing
/// changes some characters' lengths.
#[derive(Debug, Default)]
pub(crate) struct SessionPrompts {
    prompts: Vec<Prompt>,
}

#[derive(Debug, Clone)]
struct Prompt {
    text: String,
    lower: String,
    /// When it was sent, in Unix seconds, from the record's own `timestamp`.
    at: Option<i64>,
}

/// How many prompts a session's outline shows whole. A longer session shows
/// its first and its last [`OUTLINE_TAIL`], with how many were left out
/// between: the first says what it set out to do, the last where it got to.
const OUTLINE_PROMPTS: usize = 6;
const OUTLINE_TAIL: usize = 4;

/// How much of each prompt an outline line keeps, in characters. A line says
/// which prompt it was, not all of it.
const OUTLINE_CHARS: usize = 80;

/// One line of a session's outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutlineLine {
    Prompt {
        at: Option<i64>,
        text: String,
    },
    /// This many prompts between the first and the last few.
    Skipped(usize),
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
    fn push(&mut self, raw: &str, at: Option<i64>) {
        let mut text = fold_whitespace(raw);
        if text.is_empty() {
            return;
        }
        if text.len() > MAX_PROMPT_BYTES {
            let mut cut = MAX_PROMPT_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        // Codex writes one turn in two record shapes a few lines apart.
        if self.prompts.last().is_some_and(|last| last.text == text) {
            return;
        }
        let lower = lowercase_by_char(&text);
        self.prompts.push(Prompt { text, lower, at });
    }

    /// What the person wrote, in order, cut to fit a tooltip: every prompt
    /// of a short session, or the first and the last few of a long one.
    pub(crate) fn outline(&self) -> Vec<OutlineLine> {
        let line = |prompt: &Prompt| OutlineLine::Prompt {
            at: prompt.at,
            text: cut_to(&prompt.text, OUTLINE_CHARS),
        };
        if self.prompts.len() <= OUTLINE_PROMPTS {
            return self.prompts.iter().map(line).collect();
        }
        let tail = self.prompts.len() - OUTLINE_TAIL;
        std::iter::once(line(&self.prompts[0]))
            .chain(std::iter::once(OutlineLine::Skipped(tail - 1)))
            .chain(self.prompts[tail..].iter().map(line))
            .collect()
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
    let start = prompt.lower.find(needle).unwrap_or(0);
    let first = prompt.lower[..start].chars().count();
    let count = needle.chars().count();
    // Character positions carry over from the lowercase form; byte offsets do
    // not.
    let byte_at = |chars: usize| {
        prompt
            .text
            .char_indices()
            .nth(chars)
            .map_or(prompt.text.len(), |(at, _)| at)
    };
    let (start, end) = (byte_at(first), byte_at(first + count));
    let head = &prompt.text[..start];
    let matched = &prompt.text[start..end];
    let after: String = prompt.text[end..]
        .chars()
        .take(SNIPPET_TAIL_CHARS)
        .collect();
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
        after,
        prompts,
    }
}

fn cut_to(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}\u{2026}", text[..at].trim_end()),
        None => text.to_string(),
    }
}

/// A query in the form prompts are held in.
pub(crate) fn normalize_query(query: &str) -> String {
    lowercase_by_char(&fold_whitespace(query))
}

/// Lowercase, one character for one: a character whose lowercase is longer
/// than one character (`İ` is two) keeps the first of them. A search loses
/// nothing by it, and positions stay comparable with the text.
fn lowercase_by_char(text: &str) -> String {
    text.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

fn fold_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Entry {
    /// Where the next read starts: just past the last whole line read.
    read_to: u64,
    /// The file's modification time at that read. Same length and a different
    /// time is a file written over rather than appended to.
    modified: Option<std::time::SystemTime>,
    prompts: Arc<SessionPrompts>,
}

fn cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    // A panic while it was held leaves nothing half-written that matters: an
    // entry is replaced whole. Losing the cache to it would mean reading every
    // transcript from its start on every open.
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Drop what was read from files that are not in `keep` - sessions of a
/// project that was closed, or files that are gone - so the cache holds the
/// sessions the palette can list and no more.
pub(crate) fn forget_all_but(keep: &std::collections::HashSet<PathBuf>) {
    cache().retain(|path, _| keep.contains(path));
}

/// What was last read from `path`, without reading it. For a render pass,
/// which may not touch the disk; [`prompts_of`] brings it up to date.
pub(crate) fn cached(path: &Path) -> Option<Arc<SessionPrompts>> {
    cache().get(path).map(|entry| entry.prompts.clone())
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
    let Ok(meta) = std::fs::metadata(path) else {
        return Arc::default();
    };
    let (len, modified) = (meta.len(), meta.modified().ok());
    let previous = cache()
        .get(path)
        .map(|entry| (entry.read_to, entry.modified, entry.prompts.clone()));
    let (start, mut session) = match previous {
        Some((read_to, at, prompts)) if read_to == len && at == modified => return prompts,
        Some((read_to, _, prompts)) if read_to < len => (
            read_to,
            SessionPrompts {
                prompts: prompts.prompts.clone(),
            },
        ),
        _ => (0, SessionPrompts::default()),
    };
    let read_to = read_prompts(path, start, extract, &mut session).unwrap_or(start);
    let session = Arc::new(session);
    cache().insert(
        path.to_path_buf(),
        Entry {
            read_to,
            modified,
            prompts: session.clone(),
        },
    );
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
        // An error from here on returns how far was read: the prompts before
        // it are already in `session`, and starting over would add them twice.
        let Ok(read) = (&mut reader)
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut line)
        else {
            return Some(offset);
        };
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
                let Ok(chunk) = reader.fill_buf() else {
                    return Some(offset);
                };
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
            // Both stores date every record in RFC 3339.
            let at = value
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map(|when| when.timestamp());
            session.push(&text, at);
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

    fn claude_prompt_at(text: &str, timestamp: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": timestamp,
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

    /// What the CLI writes as a `user` record on its own account - a slash
    /// command, its output, its bookkeeping - is not something the person
    /// wrote, though it sits in the same record type.
    #[test]
    fn the_clis_own_user_records_are_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        let meta = serde_json::json!({
            "type": "user",
            "isMeta": true,
            "message": {"role": "user", "content": "Caveat: generated while running local commands"}
        })
        .to_string();
        let path = write(
            &dir,
            "i.jsonl",
            &[
                meta,
                claude_prompt("<command-name>/compact</command-name>"),
                claude_prompt("<local-command-stdout>Compacted</local-command-stdout>"),
            ],
        );
        let prompts = prompts_of(SessionAgent::Claude, &path);
        assert!(prompts.find("caveat").is_none());
        assert!(prompts.find("compact").is_none());
    }

    /// Lowercasing changes the byte length of some characters - the Kelvin sign
    /// is three bytes and `k` one - so a match found in the lowercase form has
    /// to be placed in the text by character, not by byte.
    #[test]
    fn a_match_after_a_character_whose_lowercase_is_shorter_is_placed_right() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "j.jsonl",
            &[claude_prompt("300 \u{212A} then Deploy notes")],
        );
        let hit = prompts_of(SessionAgent::Claude, &path)
            .find("deploy")
            .unwrap();
        assert_eq!(hit.matched, "Deploy");
        assert_eq!(hit.before, "300 \u{212A} then ");
    }

    #[test]
    fn a_snippet_keeps_only_the_start_of_a_long_tail() {
        let dir = tempfile::tempdir().unwrap();
        let tail = "x".repeat(10_000);
        let path = write(&dir, "k.jsonl", &[claude_prompt(&format!("needle {tail}"))]);
        let hit = prompts_of(SessionAgent::Claude, &path)
            .find("needle")
            .unwrap();
        assert_eq!(hit.after.chars().count(), SNIPPET_TAIL_CHARS);
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

    #[test]
    fn a_short_session_is_outlined_whole_with_its_times() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "l.jsonl",
            &[
                claude_prompt_at("set up\nthe deploy", "2026-10-23T09:15:00Z"),
                claude_prompt("no time on this one"),
            ],
        );
        assert_eq!(
            prompts_of(SessionAgent::Claude, &path).outline(),
            vec![
                OutlineLine::Prompt {
                    at: Some(1_792_746_900),
                    text: "set up the deploy".into()
                },
                OutlineLine::Prompt {
                    at: None,
                    text: "no time on this one".into()
                },
            ]
        );
    }

    #[test]
    fn a_long_session_keeps_its_first_prompt_and_its_last_few() {
        let dir = tempfile::tempdir().unwrap();
        let lines: Vec<String> = (1..=10).map(|n| claude_prompt(&format!("p{n}"))).collect();
        let path = write(&dir, "m.jsonl", &lines);
        let texts: Vec<String> = prompts_of(SessionAgent::Claude, &path)
            .outline()
            .into_iter()
            .map(|line| match line {
                OutlineLine::Prompt { text, .. } => text,
                OutlineLine::Skipped(n) => format!("+{n}"),
            })
            .collect();
        assert_eq!(texts, ["p1", "+5", "p7", "p8", "p9", "p10"]);
    }

    #[test]
    fn an_outline_line_is_cut_on_a_character() {
        let long = "й".repeat(OUTLINE_CHARS + 5);
        let cut = cut_to(&long, OUTLINE_CHARS);
        assert_eq!(cut.chars().count(), OUTLINE_CHARS + 1);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn the_summary_a_compaction_writes_is_not_the_persons() {
        let dir = tempfile::tempdir().unwrap();
        let summary = serde_json::json!({
            "type": "user",
            "isCompactSummary": true,
            "isVisibleInTranscriptOnly": true,
            "message": {"role": "user", "content": "This session is being continued from a previous conversation"}
        })
        .to_string();
        let path = write(&dir, "n.jsonl", &[summary, claude_prompt("go on")]);
        let prompts = prompts_of(SessionAgent::Claude, &path);
        assert!(prompts.find("continued").is_none());
        assert_eq!(prompts.outline().len(), 1);
    }
}
