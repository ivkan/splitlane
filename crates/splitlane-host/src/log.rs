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
//!
//! # Where the ring may be cut
//!
//! A replay that starts at the head of a ring that has wrapped must not
//! start inside an escape sequence, a UTF-8 character or a synchronized
//! update, and it needs to be told which terminal modes were in effect
//! there. Every byte therefore passes through a [`Scanner`] on its way in.
//! Where the scanner says the stream may be cut, a record is closed and the
//! next one remembers the modes it starts in; the head only ever moves to
//! the start of such a record. [`SessionLog::head_preamble`] is those modes
//! as bytes for the emulator of whoever starts reading at the head.
//!
//! Output that offers no such place - megabytes of one device control
//! string, a synchronized update that is never closed - cannot hold memory
//! for ever. Once the log has grown to twice its capacity it is cut at a
//! record boundary regardless, and says so through
//! [`SessionLog::head_torn`] until the head reaches a clean place again.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::scan::{Modes, Scanner};

/// Default capacity of one session's log.
pub const DEFAULT_CAPACITY: usize = 8 * 1024 * 1024;
/// Largest chunk consecutive writes are merged into.
pub const MAX_CHUNK: usize = 64 * 1024;
/// How much output a record holds before it is closed at the next place the
/// stream may be cut. Smaller records let the head land closer to where a
/// trim wants it, at the price of more of them.
pub const SEAL_AT: usize = 4 * 1024;

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
    /// The modes in effect where the record starts.
    modes: Arc<Modes>,
    /// Whether the stream may be cut where the record starts. The head
    /// moves to this record by choice only when it may.
    clean: bool,
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
    seal_at: usize,
    /// Terminal size in effect at `head_seq`. A trimmed resize record is
    /// folded in here, so a replay that starts at the head still knows the
    /// size to start from.
    head_size: (u16, u16),
    /// Has read everything up to `tail_seq`.
    scanner: Scanner,
    /// The last output record ends where the stream may be cut and is not
    /// added to: the next output starts a record of its own.
    sealed: bool,
    /// The modes in effect at `head_seq`. `None` while the head is the
    /// session's start, where there is nothing to restore.
    head_modes: Option<Arc<Modes>>,
    head_torn: bool,
}

impl SessionLog {
    pub fn new(capacity: usize, cols: u16, rows: u16) -> Self {
        // A chunk is dropped whole, so a chunk as large as the ring would
        // make every trim empty it. An eighth keeps a full ring at least
        // seven-eighths full after a trim.
        let chunk_cap = (capacity / 8).clamp(1, MAX_CHUNK);
        Self {
            entries: VecDeque::new(),
            head_seq: 0,
            tail_seq: 0,
            stored: 0,
            capacity,
            chunk_cap,
            seal_at: SEAL_AT.min(chunk_cap / 2).max(1),
            head_size: (cols, rows),
            scanner: Scanner::new(),
            sealed: false,
            head_modes: None,
            head_torn: false,
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

    /// The modes in effect after everything appended so far.
    pub fn modes(&self) -> &Modes {
        self.scanner.modes()
    }

    /// True when the head is not a place the stream may be cut: the log had
    /// to drop output that offered none. A replay from the head then starts
    /// with the rest of whatever was cut through.
    pub fn head_torn(&self) -> bool {
        self.head_torn
    }

    /// What an emulator has to be fed before a replay from the head:
    /// see [`Modes::preamble`]. Empty while the head is the session's
    /// start.
    pub fn head_preamble(&self) -> Vec<u8> {
        match &self.head_modes {
            Some(modes) => modes.preamble(),
            None => Vec::new(),
        }
    }

    /// Append output. Returns true when the head moved to make room.
    pub fn append_output(&mut self, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            let room = match self.entries.back() {
                Some(Entry {
                    record: Record::Output(last),
                    ..
                }) if !self.sealed => self.chunk_cap.saturating_sub(last.len()),
                _ => 0,
            };
            if room == 0 {
                self.entries.push_back(Entry {
                    seq: self.tail_seq,
                    record: Record::Output(Vec::new()),
                    modes: self.scanner.snapshot(),
                    clean: self.scanner.is_safe(),
                });
                self.sealed = false;
                continue;
            }
            // The scanner sees exactly what goes into this record, so the
            // place it reports can be compared with the record's length.
            let take = room.min(bytes.len());
            let (piece, rest) = bytes.split_at(take);
            bytes = rest;
            let held = self.chunk_cap - room;
            let cut = self
                .scanner
                .feed(piece)
                .filter(|safe| held + safe.offset >= self.seal_at);
            let Some(Entry {
                record: Record::Output(last),
                ..
            }) = self.entries.back_mut()
            else {
                continue;
            };
            match cut {
                Some(safe) => {
                    last.extend_from_slice(&piece[..safe.offset]);
                    // Closed for good; give back what growing it reserved.
                    last.shrink_to_fit();
                    if safe.offset < take {
                        self.entries.push_back(Entry {
                            seq: self.tail_seq + safe.offset as u64,
                            record: Record::Output(piece[safe.offset..].to_vec()),
                            modes: safe.modes,
                            clean: true,
                        });
                    } else {
                        self.sealed = true;
                    }
                }
                None => last.extend_from_slice(piece),
            }
            self.tail_seq += take as u64;
            self.stored += take;
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
            modes: self.scanner.snapshot(),
            // A resize is a place to cut whenever the output before it is.
            clean: self.scanner.is_safe(),
        });
        self.sealed = false;
        self.tail_seq += 1;
        self.trim_to(self.capacity) > 0
    }

    /// Drop records from the head until at most `limit` bytes are held, or
    /// until there is no place left to cut. Returns how many sequence
    /// numbers were dropped.
    ///
    /// The head stops only where the stream may be cut, so the log can stay
    /// above `limit`. It does not stay above twice its capacity: past that
    /// it is cut at a record boundary and the head is marked torn.
    pub fn trim_to(&mut self, limit: usize) -> u64 {
        let before = self.head_seq;
        while self.stored > limit {
            match self.droppable_head_records(limit == 0) {
                0 if self.stored > self.capacity.saturating_mul(2) => {
                    while self.stored > limit && self.drop_head_record() {}
                    break;
                }
                0 => break,
                count => {
                    for _ in 0..count {
                        self.drop_head_record();
                    }
                }
            }
        }
        if self.head_seq != before {
            self.settle_head();
        }
        self.head_seq - before
    }

    /// Drop everything, clean place or not. For a log nobody will read
    /// again.
    pub fn clear(&mut self) {
        while self.drop_head_record() {}
        self.settle_head();
    }

    /// How many records to drop from the head to reach the nearest place
    /// the stream may be cut. Zero when there is none.
    fn droppable_head_records(&self, to_nothing: bool) -> usize {
        let next_clean = self.entries.iter().skip(1).position(|entry| entry.clean);
        match next_clean {
            Some(skipped) => skipped + 1,
            // The end of the log can be such a place too, but cutting there
            // keeps nothing, and that includes output a client following
            // the session has not read yet. It is used only when nothing is
            // to be kept; otherwise the next record to arrive starts clean
            // and the head moves then.
            None if to_nothing && self.scanner.is_safe() => self.entries.len(),
            None => 0,
        }
    }

    fn drop_head_record(&mut self) -> bool {
        let Some(entry) = self.entries.pop_front() else {
            return false;
        };
        self.stored -= entry.record.cost();
        self.head_seq = entry.seq + entry.record.span();
        if let Record::Resize { cols, rows } = entry.record {
            self.head_size = (cols, rows);
        }
        true
    }

    /// Record what the head is after records were dropped.
    fn settle_head(&mut self) {
        let (modes, clean) = match self.entries.front() {
            Some(entry) => (entry.modes.clone(), entry.clean),
            None => {
                self.sealed = false;
                (self.scanner.snapshot(), self.scanner.is_safe())
            }
        };
        self.head_modes = Some(modes);
        self.head_torn = !clean;
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
        // Not one record per write, and not one record for all of them
        // either: a record is closed once it holds enough to be worth
        // trimming on its own.
        assert_eq!(log.entries.len(), 10_000 / SEAL_AT + 1);

        // One large write is cut at the chunk size.
        log.append_output(&vec![b'x'; MAX_CHUNK * 2]);
        assert_eq!(log.entries.len(), 10_000 / SEAL_AT + 3);
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

    use crate::scan::tests::{synthetic, Mix, Rng};

    fn preamble_text(log: &SessionLog) -> String {
        String::from_utf8_lossy(&log.head_preamble()).replace('\x1b', "^")
    }

    #[test]
    fn the_head_only_moves_to_where_the_stream_may_be_cut() {
        let (mut all_moves, mut torn) = (0, 0);
        for seed in 1..=30u64 {
            let stream = synthetic(seed, 2000, Mix::EVERYTHING);
            let mut rng = Rng(seed | 1);
            let capacity = [16, 64, 256, 1024][rng.below(4)];
            let mut log = SessionLog::new(capacity, 80, 24);
            // Sequence numbers of the resize records, to turn a position
            // of the log back into an offset of the output.
            let mut resizes: Vec<u64> = Vec::new();
            let mut fed = 0;
            let mut moves = 0;
            while fed < stream.bytes.len() {
                let before = log.head_seq();
                if rng.below(20) == 0 {
                    resizes.push(log.tail_seq());
                    log.append_resize(80 + rng.below(40) as u16, 24);
                } else {
                    // Any size of write, across any boundary.
                    let take = (1 + rng.below(capacity)).min(stream.bytes.len() - fed);
                    log.append_output(&stream.bytes[fed..fed + take]);
                    fed += take;
                }
                assert!(log.stored_bytes() <= capacity * 2 + RESIZE_COST);
                if log.head_seq() == before {
                    continue;
                }
                moves += 1;
                torn += usize::from(log.head_torn());
                let head = log.head_seq();
                let offset =
                    (head - resizes.iter().filter(|seq| **seq < head).count() as u64) as usize;
                assert_eq!(
                    stream.safe.binary_search(&offset).is_ok(),
                    !log.head_torn(),
                    "seed {seed}, capacity {capacity}: the head is at offset {offset}"
                );
                // The modes handed out with the head are the ones a
                // scanner of its own finds there, torn or not.
                {
                    let mut scanner = Scanner::new();
                    scanner.feed(&stream.bytes[..offset]);
                    assert_eq!(
                        log.head_preamble(),
                        scanner.modes().preamble(),
                        "seed {seed}, offset {offset}"
                    );
                }
                // What is left reads back as the output from there on.
                let (items, _) = drain(&log, head, 1 + rng.below(capacity));
                assert_eq!(output_of(&items), &stream.bytes[offset..fed]);
            }
            assert!(moves > 0, "seed {seed}: the head never moved");
            all_moves += moves;
        }
        assert!(all_moves > 1000, "the head moved only {all_moves} times");
        // Both kinds of cut were seen: the smallest logs are smaller than
        // a synchronized update.
        assert!(
            torn > 0 && torn < all_moves / 10,
            "{torn} of {all_moves} torn"
        );
    }

    #[test]
    fn a_log_with_no_place_to_cut_waits_and_then_cuts_anyway() {
        let mut log = SessionLog::new(1024, 80, 24);
        log.append_output(b"\x1b[?1049h\x1b[?2004hbefore");
        // One device control string, far larger than the log.
        log.append_output(b"\x1bP");
        let mut data = 0;
        while log.head_seq() == 0 {
            log.append_output(&[b'x'; 100]);
            data += 100;
            assert!(log.stored_bytes() <= 2 * 1024 + 100);
        }
        // It grew to twice its capacity first.
        assert!(data > 1024, "cut after only {data} bytes");
        assert!(log.head_torn());
        assert!(log.stored_bytes() <= 1024);
        // The modes at the cut are known all the same.
        assert!(preamble_text(&log).contains("^[?1049h"));
        assert!(preamble_text(&log).contains("^[?2004h"));

        // It stays torn for as long as the string runs, and stays bounded.
        for _ in 0..100 {
            log.append_output(&[b'x'; 100]);
            assert!(log.head_torn());
            assert!(log.stored_bytes() <= 2 * 1024 + 100);
        }
        // A resize in the middle of the string is not a place to cut.
        log.append_resize(100, 30);
        log.append_output(&[b'x'; 2000]);
        assert!(log.head_torn());

        // Once the string ends, the next trim lands on a clean head again.
        log.append_output(b"\x1b\\\x1b[?2004l");
        log.append_output(&[b'y'; 2000]);
        assert!(!log.head_torn());
        assert!(log.stored_bytes() <= 1024);
        assert!(preamble_text(&log).contains("^[?2004l"));
        let (items, _) = drain(&log, log.head_seq(), 64);
        assert!(output_of(&items).iter().all(|byte| *byte == b'y'));
        assert_eq!(log.size_at(log.head_seq()), (100, 30));
    }

    #[test]
    fn a_synchronized_update_is_kept_whole() {
        let mut log = SessionLog::new(1024, 80, 24);
        log.append_output(&[b'a'; 900]);
        log.append_output(b"\x1b[?2026h");
        log.append_output(&[b'b'; 1100]);
        // Over capacity, and the only places to cut are before the update.
        assert!(log.stored_bytes() > 1024);
        let head = log.head_seq() as usize;
        assert!(head <= 900, "the head is inside the update, at {head}");
        assert!(!log.head_torn());
        log.append_output(b"\x1b[?2026l");
        log.append_output(&[b'c'; 600]);
        assert!(log.stored_bytes() <= 1024);
        assert!(!log.head_torn());
    }

    #[test]
    fn a_cut_asked_for_from_outside_obeys_the_same_rule() {
        // What the total budget does to the largest log.
        let mut log = SessionLog::new(64 * 1024, 80, 24);
        log.append_output(&[b'a'; 6000]);
        log.append_output(b"\x1b]2;");
        log.append_output(&[b't'; 20_000]);
        let stored = log.stored_bytes();

        // Asked to halve: everything before the open string can go and
        // nothing of the string can.
        let dropped = log.trim_to(stored / 2);
        assert!(dropped > 0 && dropped <= 6000, "dropped {dropped}");
        assert!(log.stored_bytes() > stored / 2);
        assert!(!log.head_torn());
        // Asked again, there is nothing more to give.
        assert_eq!(log.trim_to(stored / 2), 0);

        log.append_output(b"\x07after");
        log.append_output(&[b'z'; 5000]);
        assert!(log.trim_to(4096) > 0);
        assert!(log.stored_bytes() <= 4096);
        assert!(!log.head_torn());

        // Forgetting a session takes everything, wherever that leaves it.
        log.append_output(b"\x1b[");
        log.clear();
        assert_eq!((log.stored_bytes(), log.head_seq()), (0, log.tail_seq()));
        assert!(log.head_torn());
    }

    #[test]
    fn there_is_no_preamble_until_the_head_leaves_the_start() {
        let mut log = SessionLog::new(64, 80, 24);
        log.append_output(b"\x1b[?2004h");
        assert_eq!(log.head_seq(), 0);
        assert!(log.head_preamble().is_empty());
        assert!(!log.head_torn());
        log.append_output(&[b'x'; 200]);
        assert!(log.head_seq() > 0);
        assert!(preamble_text(&log).contains("^[?2004h"));
    }
}
