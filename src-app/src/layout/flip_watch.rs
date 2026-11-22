//! Notices a pane whose measured width will not settle.
//!
//! A split once flickered between two layouts frame after frame: the slots
//! sized by their ratio, then by their headers, then by their ratio again.
//! The header picks what it shows from the width the previous frame measured,
//! so anything that lets the header's content move the slot can turn into a
//! loop like that. Nothing on disk said so afterwards. This watches each
//! header's width as the layout reports it and writes one warning to the log
//! when the width keeps returning to the value it had two frames before, which
//! is what a two-frame loop looks like and what a resize drag does not do.
//!
//! It runs on the render thread once per pane per frame, so it keeps two
//! samples and a counter and does nothing else until it has something to say.

use std::time::{Duration, Instant};

/// Back-and-forth steps in a row before it counts as a loop. A divider drag
/// that reverses direction makes one or two; a feedback loop makes one per
/// frame, so it reaches this within a tenth of a second at 60 frames a second.
const FLIPS_TO_REPORT: u32 = 6;
/// The flips have to come this close together. Frames are only drawn when
/// something changes, so a slow back-and-forth (a person nudging a divider
/// for a minute) is not the loop this is for.
const WINDOW: Duration = Duration::from_secs(2);
/// At most one warning per pane in this long, so a loop that comes and goes
/// does not fill the log.
const QUIET_AFTER_REPORT: Duration = Duration::from_secs(60);
/// Widths closer than this are the same width; layout rounds to device
/// pixels, which can be half a point apart.
const SAME_WIDTH: f32 = 0.75;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    width: f32,
    level: u8,
}

impl Sample {
    fn same_as(self, other: Sample) -> bool {
        self.level == other.level && (self.width - other.width).abs() < SAME_WIDTH
    }
}

/// What the warning says: the two states the pane went back and forth
/// between, and how many times.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FlipReport {
    pub width_a: f32,
    pub level_a: u8,
    pub width_b: f32,
    pub level_b: u8,
    pub flips: u32,
    pub over: Duration,
}

#[derive(Default)]
pub(crate) struct FlipWatch {
    last: Option<Sample>,
    before_last: Option<Sample>,
    flips: u32,
    first_flip_at: Option<Instant>,
    /// Set once this episode has been reported, cleared when it ends.
    reported: bool,
    last_report_at: Option<Instant>,
}

impl FlipWatch {
    /// Record one frame's width and the header level chosen from it.
    pub(crate) fn observe(&mut self, width: f32, level: u8, now: Instant) -> Option<FlipReport> {
        let sample = Sample { width, level };
        let Some(last) = self.last else {
            self.last = Some(sample);
            return None;
        };
        if sample.same_as(last) {
            // Settled: the episode, if there was one, is over. The samples
            // stay as they are so the next change is compared with them.
            self.end_episode();
            return None;
        }
        let returned = self.before_last.is_some_and(|b| sample.same_as(b));
        self.before_last = Some(last);
        self.last = Some(sample);
        if !returned {
            self.end_episode();
            return None;
        }

        let first = *self.first_flip_at.get_or_insert(now);
        if now.duration_since(first) > WINDOW {
            // Back and forth, but too slowly to be a loop: start counting
            // again from here.
            self.flips = 0;
            self.first_flip_at = Some(now);
        }
        self.flips += 1;

        let quiet = self
            .last_report_at
            .is_none_or(|at| now.duration_since(at) >= QUIET_AFTER_REPORT);
        if self.flips < FLIPS_TO_REPORT || self.reported || !quiet {
            return None;
        }
        self.reported = true;
        self.last_report_at = Some(now);
        Some(FlipReport {
            width_a: last.width,
            level_a: last.level,
            width_b: sample.width,
            level_b: sample.level,
            flips: self.flips,
            over: now.duration_since(first),
        })
    }

    fn end_episode(&mut self) {
        self.flips = 0;
        self.first_flip_at = None;
        self.reported = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(watch: &mut FlipWatch, start: Instant, samples: &[(f32, u8)]) -> Vec<FlipReport> {
        samples
            .iter()
            .enumerate()
            .filter_map(|(i, &(width, level))| {
                watch.observe(width, level, start + Duration::from_millis(16 * i as u64))
            })
            .collect()
    }

    #[test]
    fn a_two_frame_loop_is_reported_once() {
        let mut watch = FlipWatch::default();
        let loop_frames: Vec<(f32, u8)> = (0..40)
            .map(|i| if i % 2 == 0 { (326.5, 1) } else { (249.0, 0) })
            .collect();
        let reports = frames(&mut watch, Instant::now(), &loop_frames);
        assert_eq!(reports.len(), 1, "one warning per episode: {reports:?}");
        let report = reports[0];
        assert_eq!(report.flips, FLIPS_TO_REPORT);
        assert!(
            [report.width_a, report.width_b].contains(&326.5)
                && [report.width_a, report.width_b].contains(&249.0)
        );
    }

    #[test]
    fn a_drag_is_not_a_loop() {
        let mut watch = FlipWatch::default();
        // Out and back once, as a person dragging a divider does.
        let drag: Vec<(f32, u8)> = (0..30)
            .chain((0..30).rev())
            .map(|i| (300.0 + i as f32 * 5.0, 1))
            .collect();
        assert!(frames(&mut watch, Instant::now(), &drag).is_empty());
    }

    #[test]
    fn a_settled_pane_ends_the_episode_but_the_log_stays_quiet() {
        let mut watch = FlipWatch::default();
        let start = Instant::now();
        let mut samples: Vec<(f32, u8)> = (0..10)
            .map(|i| if i % 2 == 0 { (400.0, 1) } else { (300.0, 0) })
            .collect();
        samples.extend([(300.0, 0); 5]);
        samples.extend((0..10).map(|i| if i % 2 == 0 { (400.0, 1) } else { (300.0, 0) }));
        // The second loop comes well inside the quiet period.
        assert_eq!(frames(&mut watch, start, &samples).len(), 1);

        // After the quiet period a new episode is reported again.
        let later = start + QUIET_AFTER_REPORT + Duration::from_secs(1);
        let again: Vec<(f32, u8)> = (0..10)
            .map(|i| if i % 2 == 0 { (400.0, 1) } else { (300.0, 0) })
            .collect();
        assert_eq!(frames(&mut watch, later, &again).len(), 1);
    }

    #[test]
    fn slow_back_and_forth_is_not_a_loop() {
        let mut watch = FlipWatch::default();
        let start = Instant::now();
        for i in 0..20u64 {
            let (width, level) = if i % 2 == 0 { (400.0, 1) } else { (300.0, 0) };
            let at = start + Duration::from_millis(900 * i);
            assert!(watch.observe(width, level, at).is_none(), "frame {i}");
        }
    }
}
