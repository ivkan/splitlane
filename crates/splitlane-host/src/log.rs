//! The output log of one session.
//!
//! A window that attaches late rebuilds the screen by feeding the session's
//! raw output through its own terminal emulator, so the host keeps that
//! output rather than a rendered screen. The log is a bounded ring of
//! records in arrival order. Output and resizes share one sequence: each
//! byte of output takes one number and each resize record takes one. That
//! makes "everything from N" a complete question, and it puts a resize
//! exactly where it happened between two stretches of output, which is where
//! a replaying emulator has to apply it.

use std::collections::VecDeque;

/// Default capacity of one session's log.
pub const DEFAULT_CAPACITY: usize = 8 * 1024 * 1024;
/// Largest chunk consecutive writes are merged into.
pub const MAX_CHUNK: usize = 64 * 1024;

/// Most items one [`SessionLog::read_from`] returns. Resize records carry no
/// bytes, so the byte limit alone would let a long run of them be copied in
/// one piece while the log is locked.
const MAX_BATCH_ITEMS: usize = 1024;

/// What a resize record is charged against the capacity. It holds no bytes,
/// but a log of nothing but resizes must still be bounded.
const RESIZE_COST: usize = 16;

/// One entry of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

impl Record {
    /// How many sequence numbers the record takes.
    fn span(&self) -> u64 {
        match self {
            Record::Output(bytes) => bytes.len() as u64,
            Record::Resize { .. } => 1,
        }
    }

    fn cost(&self) -> usize {
        match self {
            Record::Output(bytes) => bytes.len(),
            Record::Resize { .. } => RESIZE_COST,
        }
    }
}

#[derive(Debug)]
struct Entry {
    /// Sequence number of the record's first position.
    seq: u64,
    record: Record,
}

/// A piece of the log as handed to a reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Output { seq: u64, bytes: Vec<u8> },
    Resize { seq: u64, cols: u16, rows: u16 },
}

/// Result of [`SessionLog::read_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// Sequence number of the first item, or where the next one will be.
    pub start_seq: u64,
    /// True when the requested position had already been trimmed away.
    pub gap: bool,
    pub items: Vec<Item>,
    /// Where to read next.
    pub next_seq: u64,
}

#[derive(Debug)]
pub struct SessionLog {
    entries: VecDeque<Entry>,
    /// Sequence number of the oldest position still held.
    head_seq: u64,
    /// Sequence number the next byte or record gets.
    tail_seq: u64,
    stored: usize,
    capacity: usize,
    chunk_cap: usize,
    /// Terminal size in effect at `head_seq`. A trimmed resize record is
    /// folded in here, so a replay that starts at the head still knows the
    /// size to start from.
    head_size: (u16, u16),
}

impl SessionLog {
    pub fn new(capacity: usize, cols: u16, rows: u16) -> Self {
        Self {
            entries: VecDeque::new(),
            head_seq: 0,
            tail_seq: 0,
            stored: 0,
            capacity,
            // A chunk is dropped whole, so a chunk as large as the ring
            // would make every trim empty it. An eighth keeps a full ring at
            // least seven-eighths full after a trim.
            chunk_cap: (capacity / 8).clamp(1, MAX_CHUNK),
            head_size: (cols, rows),
        }
    }

    pub fn head_seq(&self) -> u64 {
        self.head_seq
    }

    pub fn tail_seq(&self) -> u64 {
        self.tail_seq
    }

    /// Bytes charged against the capacity right now.
    pub fn stored_bytes(&self) -> usize {
        self.stored
    }

    /// Append output. Returns true when the head moved to make room.
    pub fn append_output(&mut self, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            let room = match self.entries.back() {
                Some(Entry {
                    record: Record::Output(last),
                    ..
                }) => self.chunk_cap.saturating_sub(last.len()),
                _ => 0,
            };
            if room == 0 {
                self.entries.push_back(Entry {
                    seq: self.tail_seq,
                    record: Record::Output(Vec::new()),
                });
                continue;
            }
            let take = room.min(bytes.len());
            if let Some(Entry {
                record: Record::Output(last),
                ..
            }) = self.entries.back_mut()
            {
                last.extend_from_slice(&bytes[..take]);
            }
            self.tail_seq += take as u64;
            self.stored += take;
            bytes = &bytes[take..];
        }
        self.trim_to(self.capacity) > 0
    }

    /// Append a resize record. Returns true when the head moved.
    pub fn append_resize(&mut self, cols: u16, rows: u16) -> bool {
        let record = Record::Resize { cols, rows };
        self.stored += record.cost();
        self.entries.push_back(Entry {
            seq: self.tail_seq,
            record,
        });
        self.tail_seq += 1;
        self.trim_to(self.capacity) > 0
    }

    /// Drop records from the head until at most `limit` bytes are held.
    /// Returns how many sequence numbers were dropped.
    pub fn trim_to(&mut self, limit: usize) -> u64 {
        let before = self.head_seq;
        while self.stored > limit {
            if self.droppable_head_records() == 0 {
                break;
            }
            let Some(entry) = self.entries.pop_front() else {
                break;
            };
            self.stored -= entry.record.cost();
            self.head_seq = entry.seq + entry.record.span();
            if let Record::Resize { cols, rows } = entry.record {
                self.head_size = (cols, rows);
            }
        }
        self.head_seq - before
    }

    /// How many records may be dropped from the head right now.
    ///
    /// Every record boundary is allowed today, so a trim can leave the head
    /// in the middle of an escape sequence or inside an alternate screen,
    /// and a replay from there starts with a torn sequence and unknown
    /// terminal modes. Once the host runs an emulator of its own, this is
    /// the place that limits a trim to a point where the parser is in its
    /// ground state, and [`SessionLog::read_from`] is where the modes in
    /// effect at the new head get prepended as a preamble.
    fn droppable_head_records(&self) -> usize {
        self.entries.len()
    }

    /// Terminal size in effect at `seq`.
    pub fn size_at(&self, seq: u64) -> (u16, u16) {
        let mut size = self.head_size;
        for entry in &self.entries {
            if entry.seq >= seq {
                break;
            }
            if let Record::Resize { cols, rows } = entry.record {
                size = (cols, rows);
            }
        }
        size
    }

    /// Clamp a requested position to what the log can serve: `(start, gap)`.
    /// A position ahead of the tail is treated as the tail.
    pub fn resolve(&self, seq: u64) -> (u64, bool) {
        (seq.clamp(self.head_seq, self.tail_seq), seq < self.head_seq)
    }

    /// Copy records from `seq` on, stopping once `max_bytes` of output were
    /// copied. A position older than the head starts at the head and says
    /// so in [`Batch::gap`].
    pub fn read_from(&self, seq: u64, max_bytes: usize) -> Batch {
        let (start_seq, gap) = self.resolve(seq);
        let mut items = Vec::new();
        let mut next_seq = start_seq;
        let mut budget = max_bytes.max(1);
        let first = self
            .entries
            .partition_point(|entry| entry.seq + entry.record.span() <= start_seq);
        for entry in self.entries.iter().skip(first) {
            if items.len() >= MAX_BATCH_ITEMS {
                break;
            }
            match &entry.record {
                Record::Output(bytes) => {
                    if budget == 0 {
                        break;
                    }
                    // Only the first record can begin before `next_seq`.
                    let skip = (next_seq - entry.seq) as usize;
                    let take = (bytes.len() - skip).min(budget);
                    if take == 0 {
                        continue;
                    }
                    items.push(Item::Output {
                        seq: next_seq,
                        bytes: bytes[skip..skip + take].to_vec(),
                    });
                    next_seq += take as u64;
                    budget -= take;
                    if skip + take < bytes.len() {
                        break;
                    }
                }
                Record::Resize { cols, rows } => {
                    items.push(Item::Resize {
                        seq: entry.seq,
                        cols: *cols,
                        rows: *rows,
                    });
                    next_seq = entry.seq + 1;
                }
            }
        }
        Batch {
            start_seq,
            gap,
            items,
            next_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the whole log from `seq` in batches of `step` bytes.
    fn drain(log: &SessionLog, seq: u64, step: usize) -> (Vec<Item>, bool) {
        let first = log.read_from(seq, step);
        let gap = first.gap;
        let mut next = first.next_seq;
        let mut items = first.items;
        loop {
            let batch = log.read_from(next, step);
            assert!(!batch.gap);
            if batch.items.is_empty() {
                break;
            }
            next = batch.next_seq;
            items.extend(batch.items);
        }
        assert_eq!(next, log.tail_seq());
        (items, gap)
    }

    fn output_of(items: &[Item]) -> Vec<u8> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Output { bytes, .. } => Some(bytes.as_slice()),
                Item::Resize { .. } => None,
            })
            .collect::<Vec<_>>()
            .concat()
    }

    /// Every item starts where the previous one ended.
    fn assert_contiguous(items: &[Item], mut expected: u64) {
        for item in items {
            match item {
                Item::Output { seq, bytes } => {
                    assert_eq!(*seq, expected);
                    expected += bytes.len() as u64;
                }
                Item::Resize { seq, .. } => {
                    assert_eq!(*seq, expected);
                    expected += 1;
                }
            }
        }
    }

    #[test]
    fn output_comes_back_in_order_from_any_position() {
        let mut log = SessionLog::new(1024, 80, 24);
        log.append_output(b"hello ");
        log.append_output(b"world");
        assert_eq!(log.tail_seq(), 11);

        let (items, gap) = drain(&log, 0, 4);
        assert!(!gap);
        assert_eq!(output_of(&items), b"hello world");
        assert_contiguous(&items, 0);

        let (items, _) = drain(&log, 6, 1024);
        assert_eq!(
            items,
            [Item::Output {
                seq: 6,
                bytes: b"world".to_vec()
            }]
        );

        let at_tail = log.read_from(11, 1024);
        assert!(at_tail.items.is_empty());
        assert_eq!((at_tail.start_seq, at_tail.next_seq), (11, 11));
        // Ahead of the tail is the tail, not an error and not a gap.
        let ahead = log.read_from(500, 1024);
        assert_eq!((ahead.start_seq, ahead.gap), (11, false));
    }

    #[test]
    fn small_writes_are_merged_into_chunks_of_bounded_size() {
        let mut log = SessionLog::new(DEFAULT_CAPACITY, 80, 24);
        for _ in 0..1000 {
            log.append_output(b"0123456789");
        }
        assert_eq!(log.entries.len(), 1);

        // One large write is cut at the chunk size.
        log.append_output(&vec![b'x'; MAX_CHUNK * 2]);
        assert_eq!(log.entries.len(), 3);
        assert!(log
            .entries
            .iter()
            .all(|entry| entry.record.span() as usize <= MAX_CHUNK));
        assert_eq!(log.stored_bytes(), 10_000 + MAX_CHUNK * 2);
    }

    #[test]
    fn a_resize_sits_between_the_output_around_it() {
        let mut log = SessionLog::new(1024, 80, 24);
        log.append_output(b"ab");
        log.append_resize(100, 30);
        log.append_output(b"cd");
        log.append_output(b"ef");
        log.append_resize(120, 40);

        let (items, _) = drain(&log, 0, 1);
        assert_contiguous(&items, 0);
        let (items, _) = drain(&log, 0, 1024);
        assert_eq!(
            items,
            [
                Item::Output {
                    seq: 0,
                    bytes: b"ab".to_vec()
                },
                Item::Resize {
                    seq: 2,
                    cols: 100,
                    rows: 30
                },
                Item::Output {
                    seq: 3,
                    bytes: b"cdef".to_vec()
                },
                Item::Resize {
                    seq: 7,
                    cols: 120,
                    rows: 40
                },
            ]
        );
        assert_eq!(log.tail_seq(), 8);

        // Starting exactly on a resize record replays it.
        let (items, _) = drain(&log, 2, 1024);
        assert!(matches!(items[0], Item::Resize { seq: 2, .. }));
        // Starting just past it does not.
        let (items, _) = drain(&log, 3, 1024);
        assert!(matches!(items[0], Item::Output { seq: 3, .. }));

        assert_eq!(log.size_at(0), (80, 24));
        assert_eq!(log.size_at(2), (80, 24));
        assert_eq!(log.size_at(3), (100, 30));
        assert_eq!(log.size_at(8), (120, 40));
    }

    #[test]
    fn a_full_log_drops_whole_records_from_the_head() {
        let mut log = SessionLog::new(64, 80, 24);
        assert_eq!(log.chunk_cap, 8);
        let mut trimmed = false;
        for i in 0..100u8 {
            trimmed |= log.append_output(&[i; 4]);
        }
        assert!(trimmed);
        assert!(log.stored_bytes() <= 64);
        assert_eq!(log.tail_seq(), 400);
        // The head only ever lands on a record boundary.
        assert_eq!(log.head_seq() % 8, 0);
        assert_eq!(log.head_seq(), 400 - log.stored_bytes() as u64);
    }

    #[test]
    fn reading_behind_the_head_starts_at_the_head_and_reports_a_gap() {
        let mut log = SessionLog::new(64, 80, 24);
        let written: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        log.append_output(&written);
        let head = log.head_seq();
        assert!(head > 0);

        let batch = log.read_from(0, 1024);
        assert!(batch.gap);
        assert_eq!(batch.start_seq, head);

        let (items, gap) = drain(&log, 10, 16);
        assert!(gap);
        assert_contiguous(&items, head);
        assert_eq!(output_of(&items), &written[head as usize..]);

        // At or past the head there is no gap.
        assert!(!log.read_from(head, 16).gap);
    }

    #[test]
    fn sequence_numbers_keep_counting_across_trims() {
        let mut log = SessionLog::new(32, 80, 24);
        let mut written = Vec::new();
        let mut reader_at = 0u64;
        let mut seen = Vec::new();
        for round in 0..200u32 {
            let piece = round.to_le_bytes();
            log.append_output(&piece);
            written.extend_from_slice(&piece);
            // A reader that keeps up never sees a gap, whatever is trimmed
            // behind it.
            let batch = log.read_from(reader_at, 1024);
            assert!(!batch.gap);
            assert_eq!(batch.start_seq, reader_at);
            seen.extend(output_of(&batch.items));
            reader_at = batch.next_seq;
        }
        assert_eq!(seen, written);
        assert_eq!(log.tail_seq(), written.len() as u64);
        assert!(log.head_seq() > 0);
    }

    #[test]
    fn a_trimmed_resize_still_decides_the_size_at_the_head() {
        let mut log = SessionLog::new(64, 80, 24);
        log.append_output(&[0; 8]);
        log.append_resize(132, 50);
        log.append_output(&[1; 200]);
        assert!(log.head_seq() > 9, "the resize record must be gone");
        assert_eq!(log.size_at(log.head_seq()), (132, 50));
    }

    #[test]
    fn resize_records_alone_cannot_grow_the_log_without_bound() {
        let mut log = SessionLog::new(64, 80, 24);
        for i in 0..1000u16 {
            log.append_resize(i, i);
        }
        assert!(log.entries.len() <= 64 / RESIZE_COST);
        assert_eq!(log.tail_seq(), 1000);
        assert_eq!(log.size_at(log.tail_seq()), (999, 999));
    }

    #[test]
    fn an_explicit_trim_reports_how_far_the_head_moved() {
        let mut log = SessionLog::new(1024, 80, 24);
        log.append_output(&[0; 512]);
        assert_eq!(log.trim_to(1024), 0);
        let dropped = log.trim_to(256);
        assert_eq!(dropped, log.head_seq());
        assert!(log.stored_bytes() <= 256);
        assert_eq!(log.trim_to(0), 512 - dropped);
        assert_eq!(log.head_seq(), log.tail_seq());
    }
}
