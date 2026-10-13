//! Where a session's log ends and its present begins.
//!
//! A terminal that attaches to a session in the host is sent the session's
//! log before anything new. Replaying the log rebuilds the grid and must do
//! nothing else, so whoever feeds an emulator has to know, for every byte,
//! on which side of that line it falls - and has to be told the moment the
//! line is crossed, which can be in the middle of a chunk.
//!
//! [`StreamReplay`] keeps that position and nothing more. It takes the
//! stream's events in the order the host sent them and says what to do to
//! the terminal, as [`ReplayAction`]s in the order they must be done. It
//! knows no emulator: the one who applies the actions owns the emulator,
//! whatever scans the output on its way in, and whatever is held back
//! while the output is old.

use splitlane_host::{ExitStatus, StreamEvent};

/// How a terminal comes to follow a session in the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttachMode {
    /// The window has just started the session itself. Everything in the
    /// stream is happening now, the first prompt included, and the grid has
    /// the size the window gives it.
    Fresh,
    /// The session was running before this terminal existed. Its log is
    /// replayed first, quietly and at the sizes the log recorded. A view
    /// that shows such a terminal must not clear the grid on its first
    /// layout, as it does for a pane it started.
    Reattach,
}

/// Where an attached stream begins, as the host said in its reply.
#[derive(Clone, Copy, Debug)]
pub(super) struct StreamStart {
    pub(super) start_seq: u64,
    /// Where the log ended at the attach: the first position that is new.
    pub(super) tail_seq: Option<u64>,
    /// The stream begins later than was asked, and a preamble comes first.
    pub(super) gap: bool,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

/// What the thread feeding a sink has to do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SinkStep {
    Continue,
    /// Output was lost on the way and only the modes were restored: ask
    /// the host to make the program repaint.
    Redraw,
    /// The session ended and the terminal was told. Nothing follows.
    Ended,
}

/// One thing to do to the terminal, in the order given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReplayAction<'a> {
    /// Give the grid this size: the one the session had where the stream
    /// starts, or one the log recorded, at its place between the bytes.
    /// Only ever old sizes. A size the session takes while the terminal is
    /// live is the echo of what the window asked for, and is not passed on.
    ResizeGrid { cols: u16, rows: u16 },
    /// Run these bytes through the emulator. Never empty. Not `live` is old
    /// output: it changes the grid and acts on nothing, wakes nobody, and
    /// of what it reports only the last is worth keeping.
    Feed { bytes: &'a [u8], live: bool },
    /// The replay is over. Let go of what was held back and say once that
    /// there is something to draw. Comes at most once, and never for a
    /// terminal that was live from the start.
    GoLive,
    /// The session's child ended. Nothing follows.
    Exited(ExitStatus),
}

/// A replay in progress.
struct Pending {
    /// The first position that is not old output.
    until: u64,
    /// A preamble was part of it, so the picture is incomplete. Asked for
    /// when the replay is over: a repaint in the middle of it would be
    /// painted over.
    redraw: bool,
}

/// The position of a terminal in its session's stream, for as long as some
/// of the stream is old.
///
/// Whether the program must be asked to repaint is the answer to an event,
/// not an action: the request goes to the host, and whoever applies the
/// actions may be doing so on another thread, later.
pub(super) struct StreamReplay {
    pending: Option<Pending>,
    /// The position the stream has reached, while that matters: until the
    /// replay is over.
    position: u64,
}

impl StreamReplay {
    /// Start following a stream. A terminal that reattaches is replaying
    /// from here on, and whoever applies the actions must already treat it
    /// so: the actions that open a reattach - the grid's size, and going
    /// live if nothing is old - are given before this returns.
    pub(super) fn new(
        mode: AttachMode,
        start: StreamStart,
        apply: &mut impl FnMut(ReplayAction<'_>),
    ) -> Self {
        let pending = match mode {
            AttachMode::Fresh => None,
            AttachMode::Reattach => Some(Pending {
                // A host that does not say where its log ended gives no
                // way to tell old from new, and then nothing is old.
                until: start.tail_seq.unwrap_or(start.start_seq),
                redraw: false,
            }),
        };
        if pending.is_some() {
            // The log was laid out for the size the session had where the
            // stream starts, not for the size this terminal was built at.
            apply(ReplayAction::ResizeGrid {
                cols: start.cols,
                rows: start.rows,
            });
        }
        let mut replay = Self {
            pending,
            position: start.start_seq,
        };
        // Nothing to replay, and no preamble on its way that would have to
        // be read as part of one: live at once. Otherwise an idle session
        // would leave the terminal replaying until its next byte.
        if !start.gap {
            let _ = replay.reached(start.start_seq, apply);
        }
        replay
    }

    /// True once nothing in the stream is old any more.
    pub(super) fn is_live(&self) -> bool {
        self.pending.is_none()
    }

    /// Take one event of the stream, in the order the host sent it.
    pub(super) fn handle(
        &mut self,
        event: &StreamEvent,
        apply: &mut impl FnMut(ReplayAction<'_>),
    ) -> SinkStep {
        match event {
            StreamEvent::Output { seq, bytes } => self.output(*seq, bytes, apply),
            StreamEvent::Resized { seq, cols, rows } => {
                // Only while replaying. Live, the window decides the size
                // of its grid and this is the echo of what it asked for,
                // or one half of a redraw's nudge.
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| *seq < pending.until)
                {
                    apply(ReplayAction::ResizeGrid {
                        cols: *cols,
                        rows: *rows,
                    });
                }
                self.reached(seq.saturating_add(1), apply)
            }
            // What was lost is lost; the preamble that follows says what
            // to do about it.
            StreamEvent::Gap { resumes_at, .. } => self.reached(*resumes_at, apply),
            StreamEvent::Preamble(bytes) => {
                self.feed(bytes, apply);
                match self.pending.as_mut() {
                    Some(pending) => {
                        pending.redraw = true;
                        // A preamble has no place in the log and moves
                        // nothing. But the replay is over already if the
                        // log held nothing else, and nobody has said so
                        // yet: a stream that starts with a preamble was
                        // not looked at when it was opened.
                        self.reached(self.position, apply)
                    }
                    None => SinkStep::Redraw,
                }
            }
            StreamEvent::Exited(status) => {
                // The program that would repaint is gone.
                self.abandon(apply);
                apply(ReplayAction::Exited(*status));
                SinkStep::Ended
            }
        }
    }

    /// The stream ends here, whatever was still to come: what is on the
    /// grid is all there will be, so a replay that was under way is over.
    /// No repaint is asked for, because nobody is left to ask.
    pub(super) fn abandon(&mut self, apply: &mut impl FnMut(ReplayAction<'_>)) {
        let _ = self.finish(apply);
    }

    fn output(
        &mut self,
        seq: u64,
        bytes: &[u8],
        apply: &mut impl FnMut(ReplayAction<'_>),
    ) -> SinkStep {
        let Some(until) = self.pending.as_ref().map(|pending| pending.until) else {
            self.feed(bytes, apply);
            return SinkStep::Continue;
        };
        // One chunk can hold the last old bytes and the first new ones.
        let old = usize::try_from(until.saturating_sub(seq))
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        self.feed(&bytes[..old], apply);
        let step = self.reached(seq.saturating_add(old as u64), apply);
        self.feed(&bytes[old..], apply);
        step
    }

    fn feed(&self, bytes: &[u8], apply: &mut impl FnMut(ReplayAction<'_>)) {
        if !bytes.is_empty() {
            apply(ReplayAction::Feed {
                bytes,
                live: self.is_live(),
            });
        }
    }

    /// The stream stands at `seq`. Ends the replay if that is where the
    /// log ended.
    fn reached(&mut self, seq: u64, apply: &mut impl FnMut(ReplayAction<'_>)) -> SinkStep {
        self.position = seq;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| seq >= pending.until)
        {
            return self.finish(apply);
        }
        SinkStep::Continue
    }

    fn finish(&mut self, apply: &mut impl FnMut(ReplayAction<'_>)) -> SinkStep {
        let Some(pending) = self.pending.take() else {
            return SinkStep::Continue;
        };
        apply(ReplayAction::GoLive);
        if pending.redraw {
            SinkStep::Redraw
        } else {
            SinkStep::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`ReplayAction`] that owns its bytes, to be compared with.
    #[derive(Debug, PartialEq, Eq)]
    enum Did {
        Resize(u16, u16),
        Old(&'static str),
        Live(&'static str),
        GoLive,
        Exited(ExitStatus),
    }

    fn record(done: &mut Vec<Did>) -> impl FnMut(ReplayAction<'_>) + '_ {
        // Leaked so that a failure prints text and not byte values; a test
        // feeds a handful of short strings.
        let text =
            |bytes: &[u8]| -> &'static str { String::from_utf8_lossy(bytes).into_owned().leak() };
        move |action| {
            done.push(match action {
                ReplayAction::ResizeGrid { cols, rows } => Did::Resize(cols, rows),
                ReplayAction::Feed { bytes, live: false } => Did::Old(text(bytes)),
                ReplayAction::Feed { bytes, live: true } => Did::Live(text(bytes)),
                ReplayAction::GoLive => Did::GoLive,
                ReplayAction::Exited(status) => Did::Exited(status),
            });
        }
    }

    /// The size every stream of these tests starts at.
    const OPENING: Did = Did::Resize(40, 10);

    fn start_at(start_seq: u64, tail_seq: u64) -> StreamStart {
        StreamStart {
            start_seq,
            tail_seq: Some(tail_seq),
            gap: false,
            cols: 40,
            rows: 10,
        }
    }

    fn after_a_gap(start_seq: u64, tail_seq: u64) -> StreamStart {
        StreamStart {
            gap: true,
            ..start_at(start_seq, tail_seq)
        }
    }

    fn output(seq: u64, text: &str) -> StreamEvent {
        StreamEvent::Output {
            seq,
            bytes: text.as_bytes().to_vec(),
        }
    }

    fn resized(seq: u64, cols: u16, rows: u16) -> StreamEvent {
        StreamEvent::Resized { seq, cols, rows }
    }

    fn gap_to(resumes_at: u64) -> StreamEvent {
        StreamEvent::Gap {
            lost_from: 0,
            resumes_at,
        }
    }

    fn preamble(text: &str) -> StreamEvent {
        StreamEvent::Preamble(text.as_bytes().to_vec())
    }

    fn exit() -> ExitStatus {
        ExitStatus {
            code: Some(3),
            signal: None,
        }
    }

    /// One row of a table: an event, what it must do to the terminal, and
    /// what it must answer.
    type Row = (StreamEvent, Vec<Did>, SinkStep);

    /// Open a stream, check what opening it did, then feed it the table.
    /// Returns the tracker for what a table cannot say.
    fn follow(
        mode: AttachMode,
        start: StreamStart,
        opening: &[Did],
        table: Vec<Row>,
    ) -> StreamReplay {
        let mut done = Vec::new();
        let mut replay = StreamReplay::new(mode, start, &mut record(&mut done));
        assert_eq!(done, opening, "on opening the stream");
        for (row, (event, actions, step)) in table.into_iter().enumerate() {
            let mut done = Vec::new();
            let answered = replay.handle(&event, &mut record(&mut done));
            assert_eq!(done, actions, "row {row}: {event:?}");
            assert_eq!(answered, step, "row {row}: {event:?}");
        }
        replay
    }

    use AttachMode::{Fresh, Reattach};
    use SinkStep::{Continue, Ended, Redraw};

    #[test]
    fn a_stream_the_window_started_is_live_from_its_first_byte() {
        // The log already holds the first prompt when the attach is
        // answered: the end of the log says nothing about a fresh stream.
        let replay = follow(
            Fresh,
            start_at(0, 6),
            &[],
            vec![
                (output(0, "prompt"), vec![Did::Live("prompt")], Continue),
                // The grid is the window's: no size is passed on.
                (resized(6, 10, 5), vec![], Continue),
                (output(7, "more"), vec![Did::Live("more")], Continue),
            ],
        );
        assert!(replay.is_live());

        // Not even when the stream starts with a preamble.
        follow(
            Fresh,
            after_a_gap(500, 510),
            &[],
            vec![
                (preamble("modes"), vec![Did::Live("modes")], Redraw),
                (output(500, "x"), vec![Did::Live("x")], Continue),
            ],
        );
    }

    #[test]
    fn a_reattach_starts_at_the_size_the_stream_starts_with() {
        let replay = follow(Reattach, start_at(0, 9), &[OPENING], vec![]);
        assert!(!replay.is_live());
    }

    #[test]
    fn a_reattach_with_nothing_to_replay_is_live_at_once() {
        // An idle session: no event may ever come to end the replay.
        let replay = follow(
            Reattach,
            start_at(42, 42),
            &[OPENING, Did::GoLive],
            vec![(output(42, "hello"), vec![Did::Live("hello")], Continue)],
        );
        assert!(replay.is_live());

        // A log that ended before the stream starts is no different.
        follow(Reattach, start_at(42, 7), &[OPENING, Did::GoLive], vec![]);
    }

    #[test]
    fn a_host_that_does_not_say_where_its_log_ended_has_nothing_old() {
        let start = StreamStart {
            tail_seq: None,
            ..start_at(30, 0)
        };
        follow(
            Reattach,
            start,
            &[OPENING, Did::GoLive],
            vec![(output(30, "x"), vec![Did::Live("x")], Continue)],
        );
    }

    #[test]
    fn output_before_the_end_of_the_log_is_old_and_ends_the_replay_where_the_log_ends() {
        let replay = follow(
            Reattach,
            start_at(0, 8),
            &[OPENING],
            vec![
                (output(0, "abcd"), vec![Did::Old("abcd")], Continue),
                // The chunk that ends exactly where the log does.
                (
                    output(4, "efgh"),
                    vec![Did::Old("efgh"), Did::GoLive],
                    Continue,
                ),
                (output(8, "new"), vec![Did::Live("new")], Continue),
            ],
        );
        assert!(replay.is_live());
    }

    #[test]
    fn a_chunk_across_the_end_of_the_log_is_split_there() {
        // The stream starts in the middle of the session, as after a
        // detach, so the split is by position and not by length.
        follow(
            Reattach,
            start_at(1000, 1007),
            &[OPENING],
            vec![(
                output(1000, "before after"),
                vec![Did::Old("before "), Did::GoLive, Did::Live("after")],
                Continue,
            )],
        );
    }

    #[test]
    fn an_empty_chunk_feeds_nothing() {
        follow(
            Reattach,
            start_at(0, 4),
            &[OPENING],
            vec![
                (output(0, ""), vec![], Continue),
                (
                    output(0, "abcd"),
                    vec![Did::Old("abcd"), Did::GoLive],
                    Continue,
                ),
                (output(4, ""), vec![], Continue),
            ],
        );
    }

    #[test]
    fn a_resize_in_the_log_is_applied_and_one_after_it_is_not() {
        follow(
            Reattach,
            start_at(0, 9),
            &[OPENING],
            vec![
                (output(0, "abcd"), vec![Did::Old("abcd")], Continue),
                // A resize takes one position of its own.
                (resized(4, 60, 20), vec![Did::Resize(60, 20)], Continue),
                (
                    output(5, "efgh"),
                    vec![Did::Old("efgh"), Did::GoLive],
                    Continue,
                ),
                // Live: the echo of what the window asked for.
                (resized(9, 70, 21), vec![], Continue),
            ],
        );
    }

    #[test]
    fn a_resize_that_is_the_last_of_the_log_is_applied_and_then_ends_the_replay() {
        follow(
            Reattach,
            start_at(0, 5),
            &[OPENING],
            vec![
                (output(0, "abcd"), vec![Did::Old("abcd")], Continue),
                (
                    resized(4, 60, 20),
                    vec![Did::Resize(60, 20), Did::GoLive],
                    Continue,
                ),
            ],
        );
    }

    #[test]
    fn a_gap_moves_the_position_and_feeds_nothing() {
        // Short of the end of the log: still replaying.
        let replay = follow(
            Reattach,
            start_at(0, 100),
            &[OPENING],
            vec![
                (gap_to(60), vec![], Continue),
                (output(60, "x"), vec![Did::Old("x")], Continue),
            ],
        );
        assert!(!replay.is_live());

        // To the end of the log or past it: that is where the replay ends.
        for resumes_at in [100, 4000] {
            follow(
                Reattach,
                start_at(0, 100),
                &[OPENING],
                vec![
                    (output(0, "early"), vec![Did::Old("early")], Continue),
                    (gap_to(resumes_at), vec![Did::GoLive], Continue),
                    // The loss is live from here, and so is its repaint.
                    (preamble("modes"), vec![Did::Live("modes")], Redraw),
                ],
            );
        }
    }

    #[test]
    fn a_preamble_in_a_replay_is_old_and_its_repaint_waits_for_the_end() {
        // At the start of the stream.
        follow(
            Reattach,
            after_a_gap(500, 510),
            &[OPENING],
            vec![
                (preamble("modes"), vec![Did::Old("modes")], Continue),
                (output(500, "0123"), vec![Did::Old("0123")], Continue),
                (
                    output(504, "456789"),
                    vec![Did::Old("456789"), Did::GoLive],
                    Redraw,
                ),
                // Asked for once.
                (output(510, "x"), vec![Did::Live("x")], Continue),
            ],
        );

        // After falling behind in the middle of it. The chunk that ends
        // the replay answers with the repaint although part of it is new.
        follow(
            Reattach,
            start_at(0, 100),
            &[OPENING],
            vec![
                (gap_to(90), vec![], Continue),
                (preamble("modes"), vec![Did::Old("modes")], Continue),
                (
                    output(90, "0123456789new"),
                    vec![Did::Old("0123456789"), Did::GoLive, Did::Live("new")],
                    Redraw,
                ),
            ],
        );

        // Whatever ends the replay answers with it: a resize, a gap.
        follow(
            Reattach,
            after_a_gap(500, 501),
            &[OPENING],
            vec![
                (preamble("modes"), vec![Did::Old("modes")], Continue),
                (
                    resized(500, 60, 20),
                    vec![Did::Resize(60, 20), Did::GoLive],
                    Redraw,
                ),
            ],
        );
        follow(
            Reattach,
            after_a_gap(500, 510),
            &[OPENING],
            vec![
                (preamble("modes"), vec![Did::Old("modes")], Continue),
                (gap_to(900), vec![Did::GoLive], Redraw),
            ],
        );
    }

    #[test]
    fn a_stream_that_starts_with_a_preamble_is_not_live_until_the_preamble() {
        // Nothing is old, and still the stream opens replaying: the
        // preamble must be read as old output, so that the title it sets
        // is held and released like any other. The preamble then finds
        // the log already at its end.
        let start = after_a_gap(700, 700);
        let replay = follow(Reattach, start, &[OPENING], vec![]);
        assert!(!replay.is_live());
        follow(
            Reattach,
            start,
            &[OPENING],
            vec![
                (
                    preamble("modes"),
                    vec![Did::Old("modes"), Did::GoLive],
                    Redraw,
                ),
                (output(700, "x"), vec![Did::Live("x")], Continue),
            ],
        );
    }

    #[test]
    fn a_preamble_does_not_move_the_position() {
        // It ends a replay only where the stream already stood at the end
        // of the log. Here it does not, twice, however long it is.
        let replay = follow(
            Reattach,
            after_a_gap(500, 503),
            &[OPENING],
            vec![
                (
                    preamble("a long one"),
                    vec![Did::Old("a long one")],
                    Continue,
                ),
                (output(500, "ab"), vec![Did::Old("ab")], Continue),
                (preamble("again"), vec![Did::Old("again")], Continue),
            ],
        );
        assert!(!replay.is_live());
    }

    #[test]
    fn a_stream_that_was_to_start_with_a_preamble_and_does_not_goes_live_at_its_first_event() {
        // The position is tested by whatever comes first.
        follow(
            Reattach,
            after_a_gap(700, 700),
            &[OPENING],
            vec![(
                output(700, "x"),
                vec![Did::GoLive, Did::Live("x")],
                Continue,
            )],
        );
        // A resize at or after the end of the log is not an old one.
        follow(
            Reattach,
            after_a_gap(700, 700),
            &[OPENING],
            vec![(resized(700, 60, 20), vec![Did::GoLive], Continue)],
        );
    }

    #[test]
    fn a_preamble_when_live_asks_for_a_repaint_at_once_and_every_time() {
        follow(
            Reattach,
            start_at(42, 42),
            &[OPENING, Did::GoLive],
            vec![
                (gap_to(9000), vec![], Continue),
                (preamble("one"), vec![Did::Live("one")], Redraw),
                (gap_to(9500), vec![], Continue),
                (preamble("two"), vec![Did::Live("two")], Redraw),
                (output(9500, "x"), vec![Did::Live("x")], Continue),
            ],
        );
    }

    #[test]
    fn the_end_of_the_session_ends_a_replay_first_and_asks_for_no_repaint() {
        // A session that had ended before the window attached: its log,
        // and then its end.
        follow(
            Reattach,
            start_at(0, 50),
            &[OPENING],
            vec![
                (output(0, "old"), vec![Did::Old("old")], Continue),
                (
                    StreamEvent::Exited(exit()),
                    vec![Did::GoLive, Did::Exited(exit())],
                    Ended,
                ),
            ],
        );

        // A repaint that was waiting is dropped: the program that would
        // repaint is gone.
        follow(
            Reattach,
            after_a_gap(500, 510),
            &[OPENING],
            vec![
                (preamble("modes"), vec![Did::Old("modes")], Continue),
                (
                    StreamEvent::Exited(exit()),
                    vec![Did::GoLive, Did::Exited(exit())],
                    Ended,
                ),
            ],
        );

        // Live, there is no replay to end.
        follow(
            Fresh,
            start_at(0, 0),
            &[],
            vec![(
                StreamEvent::Exited(exit()),
                vec![Did::Exited(exit())],
                Ended,
            )],
        );
    }

    #[test]
    fn abandoning_a_stream_ends_its_replay_once() {
        let mut replay = follow(
            Reattach,
            after_a_gap(500, 510),
            &[OPENING],
            vec![(preamble("modes"), vec![Did::Old("modes")], Continue)],
        );
        let mut done = Vec::new();
        replay.abandon(&mut record(&mut done));
        replay.abandon(&mut record(&mut done));
        assert_eq!(done, [Did::GoLive]);
        assert!(replay.is_live());

        let mut replay = follow(Fresh, start_at(0, 0), &[], vec![]);
        let mut done = Vec::new();
        replay.abandon(&mut record(&mut done));
        assert_eq!(done, []);
    }
}
