//! The one shape every vendor's limits are read into.
//!
//! Two sources exist and they agree on almost nothing. Claude's is an
//! undocumented HTTP endpoint that states two windows and never says how long
//! either one is - `5-hour` and `weekly` are **our** words for them. Codex's is
//! its own rollout on disk, which states the length of every window it names
//! and is written by the agent rather than answered to us. A third, if the
//! reports about the credit-billed CLIs are right, will not speak in windows at
//! all.
//!
//! So the form is not "the union of what two vendors send". It is the four
//! things a footer has to have from **any** of them:
//!
//! 1. a number to draw and to compare, so the meter and the thresholds do not
//!    have to know whose row they are in;
//! 2. enough to name the row - who, which window, how long;
//! 3. the moment the reading stops being about the present, which is not the
//!    same question as how old it is (see [`Limit::is_current`]);
//! 4. a degraded state with a short reason, because a source that stopped
//!    answering must say so rather than disappear.
//!
//! **Written before the second vendor and not after it**, which is the whole
//! point: an adapter written after Codex would be Claude's shape with Codex
//! bolted on, and the third vendor would pay for both.
//!
//! ## What is deliberately not here yet
//!
//! **A `kind` field.** The note this track works from asks for
//! `{ utilization, resets_at, window_length, kind }` with `kind` naming window
//! / credits / balance. A field whose every value today is the same value is
//! not information, and - the sharper objection, raised by the cross-vendor
//! review of this design on 9 September - a flat record with a `kind` beside it
//! can express states that mean nothing: a credit pool with a window length, a
//! window measured in credits. When a credit source actually exists, [`Limit`]
//! becomes an enum whose variants carry their own fields, and the only thing
//! the meter ever asked of it - [`Limit::consumed_fraction`] - is already a
//! method rather than a field, so the meter, the thresholds and the choice of
//! the tightest window do not move.
//!
//! **A concurrency ceiling** ("no more than N sessions at once"). It is
//! recorded in the note as a separate question and it stays out of this form on
//! purpose: it is not consumption. Its numerator is **ours** - we count the
//! sessions - while everything here is somebody else's observation with
//! somebody else's timestamp, and its share moves down as well as up, so the
//! meter's own hysteresis would lie about it. It belongs beside the session
//! manager, not inside a `Limit`.

use crate::claude_usage::{LimitsError, UsageSnapshot};
use crate::codex_sessions::CodexAccountLimits;

/// The length this build gives Claude's session window, in minutes.
///
/// The endpoint states neither length. These two constants are the adapter
/// stating what the vendor will not, which is exactly the line between an
/// adapter and a parser: `claude_usage` reads what is sent, and this says what
/// it is known to mean. They are also what makes the window naming rule
/// (`window_name`) produce `5-hour` and `weekly` without a second table.
pub(crate) const CLAUDE_SESSION_MINUTES: i64 = 5 * 60;
/// The length of Claude's rolling weekly window, in minutes.
pub(crate) const CLAUDE_WEEKLY_MINUTES: i64 = 7 * 24 * 60;

/// How long a vendor may go without saying anything before its row goes away,
/// in seconds.
///
/// **The freshness horizon, at its widest rung.** The design's ladder tightens
/// it - 14 days, then 7, then 3, then 1 - when more rows want the footer than
/// fit in two; this build reads two vendors and can never need the ladder, so
/// it holds the top rung and the tightening is not written. What the rung
/// itself is for: somebody who ran a vendor once a fortnight ago should not
/// carry its row about, and a row that disappears for that reason says nothing
/// on its way out, because nothing is wrong.
pub(crate) const FRESHNESS_HORIZON: i64 = 14 * 24 * 60 * 60;

/// Who the limits belong to.
///
/// A closed enum rather than a string: a vendor is not a label, it is a reader,
/// a delivery model and a row, and the compiler should be the thing that
/// notices when a third one arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Vendor {
    Claude,
    Codex,
}

impl Vendor {
    /// What a person calls it. The rail's row starts with this word.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Vendor::Claude => "Claude",
            Vendor::Codex => "Codex",
        }
    }

    /// How this vendor's numbers reach us.
    pub(crate) fn delivery(self) -> Delivery {
        match self {
            Vendor::Claude => Delivery::Polled,
            Vendor::Codex => Delivery::Passive,
        }
    }
}

/// How a reading arrives, and therefore what its age means.
///
/// **The one capability worth carrying**, and it replaces three that are not.
/// "The source states the window's length" is already in the data, as
/// `Option<i64>`, and a flag beside it could only ever disagree with it.
/// "The source can tell an absent plan from an unread one" is already spent:
/// [`LimitsReading`] has separate variants for those, decided when the reply is
/// parsed. What is left cannot be derived from any field, and everything the
/// interface does with age hangs off it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// We ask. A stale reading means the source stopped answering, which is a
    /// fault, and asking again is the fix - so the row offers it.
    Polled,
    /// We overhear. The agent writes its own state as it works, and an old
    /// reading means **the agent has not run**, which is not a fault and not
    /// something a retry mends. A row like this says when the vendor last spoke
    /// rather than when we last looked, because those are the same moment for a
    /// polled source and days apart for this one.
    Passive,
}

impl Delivery {
    /// How the row introduces the moment behind its numbers.
    pub(crate) fn age_word(self) -> &'static str {
        match self {
            Delivery::Polled => "last read",
            Delivery::Passive => "last activity",
        }
    }
}

/// One limit, as its vendor states it.
///
/// Every limit either source states today is a rolling window, so this is a
/// window and says so, rather than a general shape with a discriminant that
/// only ever holds one value. See the module docs for what happens when that
/// stops being true.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Limit {
    /// Per cent of the window consumed, `0.0..=100.0`.
    pub(crate) utilization: f32,
    /// How long the window is, when the source states it or the adapter knows
    /// it for certain. `None` is not an error - it is a window whose name we
    /// have not earned, and the label says less rather than something invented.
    pub(crate) window_minutes: Option<i64>,
    /// Unix seconds at which it starts over, when stated.
    pub(crate) resets_at: Option<i64>,
}

impl Limit {
    /// The one number the meter draws, the thresholds compare and the choice of
    /// the binding limit sorts by, `0.0..=1.0`.
    ///
    /// **A method and not the stored form.** Today it is a division by a
    /// hundred and looks like ceremony; it is the seam a credit pool arrives
    /// through, where the reading is `12 400 of 25 000` and the share is the
    /// derived thing rather than the stated one.
    pub(crate) fn consumed_fraction(self) -> f32 {
        (self.utilization / 100.0).clamp(0.0, 1.0)
    }

    /// Whether this reading is still about the window it names.
    ///
    /// **The invariant that makes a disk source safe, and it is not about
    /// disks.** A rollout file is never rewritten, so a scan backwards keeps
    /// answering `95%` about a window that started over an hour ago - the
    /// defect the report warned about. But the same thing happens to a polled
    /// source read a second before its reset and drawn a second after it, and
    /// the honest statement covers both: **a window whose reset moment has
    /// passed is a record, not a reading.** One place asks, so no consumer has
    /// to remember to.
    ///
    /// **A window whose source stated no reset still cannot outlive its own
    /// length.** Claude's weekly figure arrives without a reset moment often
    /// enough to have a test of its own, and taken at face value it would have
    /// been "current" for the whole fourteen-day horizon - a seven-day window
    /// drawn as live after rolling over twice. So where the length is known,
    /// the latest the window can still be running is one length after it was
    /// observed. That is an inference and the weaker of the two rules, which is
    /// why the stated reset is asked for first.
    ///
    /// A window with neither a reset nor a length has nothing to check against
    /// and is taken at face value; its age is all that is left to judge it by,
    /// which is [`VendorLimits::age`]'s job.
    pub(crate) fn is_current(self, now: i64, observed_at: Option<i64>) -> bool {
        if let Some(resets_at) = self.resets_at {
            return resets_at > now;
        }
        match (self.window_minutes.filter(|m| *m > 0), observed_at) {
            (Some(minutes), Some(observed_at)) => {
                observed_at.saturating_add(minutes.saturating_mul(60)) > now
            }
            _ => true,
        }
    }
}

/// What one read of one vendor found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LimitsReading {
    /// What the source said, in the order it said it. May be empty - a source
    /// that answered and named no window it is still inside says nothing about
    /// the present, and that is a state of its own rather than a failure or a
    /// zero.
    Limits(Vec<Limit>),
    /// The account has no plan window at all: it is billed by an API key, and
    /// the windows are absent because there are none rather than because
    /// nothing was read. Drawn rather than hidden - a person in this mode
    /// otherwise cannot tell "this vendor is not supported" from "there is
    /// nothing to show".
    NoPlanLimit,
    /// The read failed, with the short reason the row prints.
    Unreadable(LimitsError),
}

/// One vendor's limits, as of one read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VendorLimits {
    pub(crate) vendor: Vendor,
    /// When the **vendor** stated this. Equal to `read_at` for a polled source
    /// and older - sometimes days older - for one we overhear.
    pub(crate) observed_at: Option<i64>,
    /// When we looked, in Unix seconds.
    pub(crate) read_at: i64,
    pub(crate) reading: LimitsReading,
}

impl VendorLimits {
    /// How this vendor's numbers reach us.
    pub(crate) fn delivery(&self) -> Delivery {
        self.vendor.delivery()
    }

    /// The windows this reading still stands behind, newest statement first.
    ///
    /// Everything on screen goes through here, so the "already reset" rule
    /// (see [`Limit::is_current`]) cannot be applied in one place and forgotten
    /// in another.
    pub(crate) fn current(&self, now: i64) -> Vec<Limit> {
        match &self.reading {
            LimitsReading::Limits(limits) => limits
                .iter()
                .copied()
                .filter(|limit| limit.is_current(now, self.observed_at))
                .collect(),
            LimitsReading::NoPlanLimit | LimitsReading::Unreadable(_) => Vec::new(),
        }
    }

    /// How old the statement behind this row is, in seconds, or `None` when the
    /// source never said when it was speaking.
    pub(crate) fn age(&self, now: i64) -> Option<i64> {
        self.observed_at.map(|at| now.saturating_sub(at))
    }

    /// Whether the source answered and yet has nothing to say about the
    /// present: every window it named has already started over.
    ///
    /// **A fourth state, and it needs its own name.** It is not `Unreadable` -
    /// the read worked. It is not `NoPlanLimit` - that is a standing fact about
    /// how the account is billed. It is not a zero. For a source we overhear it
    /// is the ordinary state of a Monday morning: the agent last ran on Friday,
    /// every window it mentioned has turned over since, and the only true thing
    /// to draw is when it last spoke.
    pub(crate) fn nothing_current(&self, now: i64) -> bool {
        matches!(self.reading, LimitsReading::Limits(_)) && self.current(now).is_empty()
    }
}

/// Claude's reading, in the shared shape.
///
/// The two lengths are ours (see [`CLAUDE_SESSION_MINUTES`]), and stating them
/// here rather than in the footer is what let two hardcoded strings leave the
/// interface: the row is labelled by the window's length, whoever stated it.
pub(crate) fn from_claude(
    result: &Result<UsageSnapshot, LimitsError>,
    observed_at: i64,
) -> VendorLimits {
    let reading = match result {
        Ok(snapshot) => {
            let mut limits = Vec::with_capacity(2);
            if let Some(window) = snapshot.five_hour {
                limits.push(Limit {
                    utilization: window.utilization,
                    window_minutes: Some(CLAUDE_SESSION_MINUTES),
                    resets_at: window.resets_at,
                });
            }
            if let Some(window) = snapshot.seven_day {
                limits.push(Limit {
                    utilization: window.utilization,
                    window_minutes: Some(CLAUDE_WEEKLY_MINUTES),
                    resets_at: window.resets_at,
                });
            }
            LimitsReading::Limits(limits)
        }
        Err(err) => LimitsReading::Unreadable(*err),
    };
    VendorLimits {
        vendor: Vendor::Claude,
        // We ask, so the moment the vendor spoke is the moment we heard it.
        observed_at: Some(observed_at),
        read_at: observed_at,
        reading,
    }
}

/// Codex's reading, in the shared shape.
///
/// Everything here is stated by the source: the percentage, the window's own
/// length in minutes, the reset moment. That is the half of the normalisation
/// this vendor makes easy - and the half it makes hard is the timestamp, which
/// is when **Codex** wrote the line rather than when we read it, and which is
/// therefore days old whenever the agent has not run. See [`Delivery`].
///
/// `primary` and `secondary` are upstream's names for "the two windows", in no
/// stated order of length; nothing here relies on which is which, because the
/// rules downstream sort by the length each one states.
pub(crate) fn from_codex(account: &CodexAccountLimits) -> VendorLimits {
    let (reading, observed_at) = match account {
        CodexAccountLimits::Windows {
            limits,
            observed_at,
        } => {
            let windows = [limits.primary, limits.secondary]
                .into_iter()
                .flatten()
                .map(|window| Limit {
                    // Upstream states this as an f64 and the meter reads a
                    // fraction; the clamp is the same one the other source's
                    // parser applies, for the same reason - a meter that
                    // overflows its own track is a rendering bug.
                    utilization: (window.used_percent as f32).clamp(0.0, 100.0),
                    window_minutes: window.window_minutes.filter(|minutes| *minutes > 0),
                    resets_at: window.resets_at,
                })
                .collect();
            (LimitsReading::Limits(windows), *observed_at)
        }
        CodexAccountLimits::NoPlanWindow { observed_at } => {
            (LimitsReading::NoPlanLimit, *observed_at)
        }
    };
    VendorLimits {
        vendor: Vendor::Codex,
        observed_at: Some(observed_at),
        // We overheard it now; it was said then. Only the second of those is
        // evidence about the numbers.
        read_at: observed_at,
        reading,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_usage::UsageWindow;

    fn window(utilization: f32, resets_at: Option<i64>) -> UsageWindow {
        UsageWindow {
            utilization,
            resets_at,
        }
    }

    /// The adapter states the two lengths the endpoint does not, which is what
    /// lets one naming rule label both vendors' rows.
    #[test]
    fn the_adapter_states_the_lengths_the_endpoint_never_sends() {
        let snapshot = UsageSnapshot {
            five_hour: Some(window(63.0, Some(1_000))),
            seven_day: Some(window(21.5, None)),
        };
        let vendor = from_claude(&Ok(snapshot), 500);
        let LimitsReading::Limits(limits) = &vendor.reading else {
            panic!("a good reply is a reading");
        };
        assert_eq!(limits[0].window_minutes, Some(CLAUDE_SESSION_MINUTES));
        assert_eq!(limits[1].window_minutes, Some(CLAUDE_WEEKLY_MINUTES));
        assert_eq!(limits[1].resets_at, None, "a window may state no reset");
    }

    /// A failed read is a state with a reason, never an absence of windows:
    /// those two draw differently and mean different things.
    #[test]
    fn a_failed_read_keeps_its_reason() {
        let vendor = from_claude(&Err(LimitsError::Network), 500);
        assert_eq!(
            vendor.reading,
            LimitsReading::Unreadable(LimitsError::Network)
        );
        assert!(vendor.current(500).is_empty());
        assert!(
            !vendor.nothing_current(500),
            "a read that failed has not answered, so it cannot have answered emptily"
        );
    }

    /// The rule that makes a source we overhear safe, stated on a window rather
    /// than on a source: past its reset, the number is a record.
    #[test]
    fn a_window_past_its_reset_is_a_record_and_not_a_reading() {
        let spent = Limit {
            utilization: 95.0,
            window_minutes: Some(300),
            resets_at: Some(1_000),
        };
        assert!(spent.is_current(999, Some(0)));
        assert!(
            !spent.is_current(1_000, Some(0)),
            "the reset moment is not inside it"
        );
        assert!(!spent.is_current(5_000, Some(0)));

        // No reset stated: the window still cannot outlive its own length,
        // counted from the moment it was observed. Five hours is 18 000
        // seconds.
        let undated = Limit {
            resets_at: None,
            ..spent
        };
        assert!(undated.is_current(17_999, Some(0)));
        assert!(
            !undated.is_current(18_001, Some(0)),
            "a five-hour window observed at zero is over by then, stated reset or not"
        );
        // Neither a reset nor a length: nothing to check against, so it stands.
        let unbounded = Limit {
            window_minutes: None,
            ..undated
        };
        assert!(unbounded.is_current(5_000_000, Some(0)));
        assert!(undated.is_current(5_000_000, None));
    }

    /// The fourth state: read fine, and nothing it said is about now.
    #[test]
    fn a_source_that_only_knows_about_yesterday_says_so() {
        let vendor = VendorLimits {
            vendor: Vendor::Codex,
            observed_at: Some(900),
            read_at: 5_000,
            reading: LimitsReading::Limits(vec![Limit {
                utilization: 95.0,
                window_minutes: Some(300),
                resets_at: Some(1_000),
            }]),
        };
        assert!(vendor.current(5_000).is_empty());
        assert!(vendor.nothing_current(5_000));
        assert_eq!(vendor.age(5_000), Some(4_100));
        // And the same reading, before that window turned over, is a reading.
        assert!(!vendor.nothing_current(999));
    }

    /// The second source, in the same shape: every field stated by the vendor,
    /// and the moment stated by the vendor too.
    #[test]
    fn codex_states_its_own_window_lengths_and_its_own_moment() {
        let account = CodexAccountLimits::Windows {
            limits: crate::codex_sessions::CodexRateLimits {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(crate::codex_sessions::CodexRateLimitWindow {
                    used_percent: 63.5,
                    window_minutes: Some(300),
                    resets_at: Some(2_000),
                }),
                secondary: Some(crate::codex_sessions::CodexRateLimitWindow {
                    used_percent: 82.0,
                    window_minutes: Some(10_080),
                    resets_at: None,
                }),
                plan_type: Some("plus".to_string()),
                rate_limit_reached_type: None,
            },
            observed_at: 1_000,
        };
        let vendor = from_codex(&account);
        assert_eq!(vendor.vendor, Vendor::Codex);
        assert_eq!(vendor.observed_at, Some(1_000));
        let LimitsReading::Limits(limits) = &vendor.reading else {
            panic!("windows are a reading");
        };
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].window_minutes, Some(300));
        assert_eq!(limits[1].window_minutes, Some(10_080));
        // Read an hour later, the five-hour window has already started over
        // and the weekly one - which stated no reset - has not.
        assert_eq!(vendor.current(5_000).len(), 1);
    }

    /// An API key has no plan window, and that is a row rather than a silence.
    #[test]
    fn an_api_key_login_draws_a_state_of_its_own() {
        let vendor = from_codex(&CodexAccountLimits::NoPlanWindow { observed_at: 900 });
        assert_eq!(vendor.reading, LimitsReading::NoPlanLimit);
        assert!(vendor.current(1_000).is_empty());
        assert!(
            !vendor.nothing_current(1_000),
            "a login with no plan window has not failed to answer - it answered"
        );
    }

    /// Age means different things to the two delivery models, which is why the
    /// row says a different word for it.
    #[test]
    fn a_source_we_overhear_names_the_vendors_moment_not_ours() {
        assert_eq!(Vendor::Claude.delivery(), Delivery::Polled);
        assert_eq!(Vendor::Codex.delivery(), Delivery::Passive);
        assert_eq!(Delivery::Polled.age_word(), "last read");
        assert_eq!(Delivery::Passive.age_word(), "last activity");
    }

    /// The meter's only question, and the shape it survives a credit pool in.
    #[test]
    fn the_fraction_is_derived_and_clamped() {
        let limit = Limit {
            utilization: 42.0,
            window_minutes: None,
            resets_at: None,
        };
        assert!((limit.consumed_fraction() - 0.42).abs() < f32::EPSILON);
        assert!(
            (Limit {
                utilization: 140.0,
                ..limit
            }
            .consumed_fraction()
                - 1.0)
                .abs()
                < f32::EPSILON
        );
    }
}
