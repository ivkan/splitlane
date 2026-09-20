//! The rail's limits footer: what the Claude plan has left, and when it is
//! not knowable.
//!
//! The design is explicit that this footer **never disappears** - an absent
//! footer reads as "no limits", which is a stronger and wronger claim than
//! "not read". So there is one shape with two fillings: the reading, and the
//! design's own degraded state (label "Claude limits unavailable" once a read
//! has actually failed, an em dash where the percentage goes, an empty meter).
//!
//! **Under the block stands the origin line**, in every state:
//! `read 14:32 ↻` when the reading is good - pressing it reads again - and the
//! reason when it is not, beside the one action that can answer it: `sign in`
//! for a missing or dead login, `retry now` for the failures a second attempt
//! can fix. A Claude user who has not signed in yet gets a one-line block with
//! no meter and `sign in`, which opens a Claude Code pane; somebody without the
//! CLI still gets nothing.
//!
//! **The reason is now named.** An earlier build wrote only "limits
//! unavailable · not read yet", because with no reader it could not tell "not
//! signed in" from "there is nothing here that reads them" - and naming a
//! cause would have been an assertion it had not earned. [`crate::claude_usage`]
//! establishes one, so the line says which of five things happened: not signed
//! in, expired login, rate limited, no network, bad reply. Every phrase is
//! thirteen characters or fewer, which is what makes "not signed in · last
//! read 08:12" fit the rail's default width where a longer form was cut
//! mid-word.
//!
//! **Which limit is shown has memory.** Two rolling windows bind at different
//! times, and the prototype's rule (session when it leads the week by more
//! than five points, or when it is at eighty) flips on every crossing when the
//! two values run close. [`choose_limit`] is that rule with hysteresis: the
//! session takes over at a five-point lead and gives it back only once the
//! week has actually caught up. The label always moves with the number - one
//! never changes without the other.
//!
//! Historical note: between the meter and the token line there used to be a
//! seven-bar weekly sparkline, drawn from samples this app took because the
//! endpoint carries no per-day series. It was removed: seven bars read as
//! a record of the week, and the only series obtainable is a record of when
//! the app was watching. The line below already carries the number.

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, ClickEvent, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Role,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};

use crate::SplitlaneApp;
use crate::claude_usage::{LimitsError, UsageSnapshot};
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;
use crate::vendor_limits::{Delivery, Limit, LimitsReading, Vendor, VendorLimits};

/// Height of the meter track. A physical size: a meter's thickness is neither
/// type nor rhythm.
const METER_HEIGHT: f32 = 4.0;

/// The two thresholds where the meter takes a colour, and where it takes the
/// next one. Straight from the design.
///
/// **Colour and word change together, and nothing below the first is
/// coloured.** The names are the words on screen, because they are the same
/// two facts: below `NEARLY_OUT_AT` the meter is `dim` and says nothing, at it
/// the meter warns and says `nearly out`, at `REACHED_AT` it errors and says
/// `reached`.
///
/// This replaced a scheme that was wrong in two ways at once. The healthy
/// meter was drawn in the **accent** - the one hue this app has for *something
/// wants you* - so a bar that wants nothing spent it on every screen all day;
/// that is the accent's one-question rule broken one surface over. And the old first
/// threshold sat at 70% with no word behind it, which made this the last
/// colour-only signal in the interface. There is now no such band: every
/// coloured meter has a word beside it, every word takes its meter's own
/// colour, and a reader who cannot see the hue reads the word instead.
pub(crate) const NEARLY_OUT_AT: f32 = 85.0;
pub(crate) const REACHED_AT: f32 = 100.0;

/// The lead, in percentage points, at which a shorter window takes the footer
/// from a longer one - and, being a Schmitt trigger, the width of the band it
/// then has to fall back through.
const SHORTER_TAKES_OVER_AT: f32 = 5.0;
/// A shorter window this full binds whatever the longer one is doing.
const SHORTER_LATCHES_AT: f32 = 80.0;

/// Which window the footer is showing, named by the thing both sources state
/// about a window: how long it is.
///
/// **The identity of a window is its length, not its position.** It used to be
/// `Session` / `Weekly`, which is one vendor's pair of windows written into
/// every signature that touched them - the row's memory, the slope's other
/// reading, the mark on an announcement. Two vendors state windows of their
/// own lengths, and pairing by position would have subtracted Codex's
/// `primary` from Claude's session window the day the second row appeared.
/// `None` is a window whose source stated no length: it can be drawn, but it
/// cannot be remembered, and every user of this key says what it does about
/// that.
type WindowKey = Option<i64>;

/// What a window of this length is called.
///
/// **A word only where the word is exact; a number everywhere else.** An hour,
/// a day and a week have names that mean exactly those lengths, so a window of
/// 60, 1440 or 10080 minutes takes the name. Anything else prints its own
/// number in the largest unit that divides it without remainder - 300 minutes
/// is `5-hour`, 4320 is `3-day`, 90 is `90-minute`.
///
/// Two consequences worth stating, because both are the point rather than
/// side effects.
///
/// **43200 minutes is `30-day`, never `monthly`.** A thirty-day window is not a
/// calendar month, and `monthly` would promise a reset on the first - the same
/// class of error as calling ninety minutes `hourly`, and dearer, because a
/// month is a thing people expect a date for.
///
/// **A length we have never seen gets a digit rather than a name**: a wrong
/// digit is visible on sight, a wrong name lives for years.
///
/// `None` in, `None` out: a window whose length the source did not state has no
/// name, and inventing one would be the footer asserting something it was not
/// told. Non-positive is the same answer for the same reason - it is not a
/// length.
///
/// The rule was written for the second vendor, which states `window_minutes`
/// outright, and it is used here because applied to this vendor's own two
/// windows it returns exactly the two strings that were hardcoded before it.
/// One rule now labels both rows.
fn window_name(window_minutes: Option<i64>) -> Option<String> {
    let minutes = window_minutes.filter(|m| *m > 0)?;
    const HOUR: i64 = 60;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    Some(match minutes {
        HOUR => "hourly".to_string(),
        DAY => "daily".to_string(),
        WEEK => "weekly".to_string(),
        m if m % DAY == 0 => format!("{}-day", m / DAY),
        m if m % HOUR == 0 => format!("{}-hour", m / HOUR),
        m => format!("{m}-minute"),
    })
}

/// The whole label for one limit row: who it belongs to, which window it is,
/// and the noun.
///
/// Every label in the footer is built here, so a row whose window has no name
/// cannot come out shaped differently from one whose window has: it simply
/// says less. `vendor` is `None` on the second row, where the vendor is already
/// named by the row above it.
fn limit_label(vendor: Option<&str>, window_minutes: Option<i64>) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(3);
    if let Some(vendor) = vendor {
        parts.push(vendor);
    }
    let name = window_name(window_minutes);
    match name.as_deref() {
        Some(name) => parts.push(name),
        // A window with no name still has to say **which** window it is, and
        // the two rows that can reach here want different words for it: the
        // second window inside a vendor's row is the *other* one, while a whole
        // vendor's collapsed row is that vendor's only readable limit and needs
        // no such word. The two are kept apart; they used to both say bare
        // "limit", which on the second row read as a repeat of the first.
        None if vendor.is_none() => parts.push("other"),
        None => {}
    }
    parts.push("limit");
    parts.join(" ")
}

/// Whether the limits panel has anything true to say yet.
///
/// Two states mean "nothing", and until now both drew a panel anyway:
///
/// - **not looked yet** (`None`). The first read waits
///   [`LIMITS_FIRST_READ_DELAY`] behind window creation, and for those three
///   seconds the panel asserted "Claude limits unavailable" about a session it
///   had not asked about. Every launch, to every user, including the ones whose
///   limits were about to load fine.
/// - **no account** (`NotSignedIn`), which is "no credentials anywhere the CLI
///   would have put them". For somebody who works in Codex and has never signed
///   into Claude Code, that panel was permanent, unfixable, and about a product
///   they do not own. That is not a degraded state, it is a wrong assumption
///   made visible.
///
/// Every other error stays on screen, because each one is a fact about a real
/// account: an expired login, a rate limit and a dead network are all things
/// the user can act on, and hiding them would lose the retry button with them.
///
/// **But "no account" is only a wrong assumption for somebody without the
/// CLI.** Somebody with `claude` on `PATH` and no credentials is a Claude user
/// who has not signed in yet, and hiding the panel from them hid
/// the one thing they could act on - the rail showed an empty frame and then
/// stayed empty for up to half an hour after they had signed in. So
/// `NotSignedIn` keeps its panel exactly when the CLI is installed, and that
/// panel's action is `sign in`.
pub(crate) fn limits_footer_has_something_to_say(
    state: Option<&Result<UsageSnapshot, LimitsError>>,
    cli_installed: bool,
) -> bool {
    match state {
        None => false,
        Some(Err(LimitsError::NotSignedIn)) => cli_installed,
        Some(_) => true,
    }
}

/// What the app knows about the usage limits.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClaudeLimits {
    /// Unix millis of the last attempt to read them, or `None` before the
    /// first. The design's copy is "last read 08:12"; before any attempt
    /// there is no time to name and the line says so.
    pub(crate) last_read_at: Option<u64>,
    /// The last attempt's outcome. `None` before the first one.
    pub(crate) state: Option<Result<UsageSnapshot, LimitsError>>,
    /// Which window was shown last, by its length, so the label cannot flap
    /// when two values cross. See [`choose_limit`].
    pub(crate) shown: Option<WindowKey>,
    /// A read is in flight. Guards against a second one being started by the
    /// tick, the focus or an impatient click.
    pub(crate) reading: bool,
    /// How many attempts in a row have failed, which is how far the
    /// focus-triggered read backs off.
    pub(crate) consecutive_failures: u32,
    /// The reading before the current one, per window, and when it was taken.
    ///
    /// **The slope has to come from here and cannot come from our own
    /// measurements**, and the reason is a missing conversion rather than a
    /// missing number. The server states a *share of a window*; everything this
    /// app can measure locally is *dollars*, and nothing published says how many
    /// dollars a window holds. Adding a locally-measured spend to a vendor
    /// percentage would be adding two quantities with no common unit and
    /// printing the result as a projection.
    ///
    /// So the rate of consumption is `(now - then)` over two readings of the
    /// vendor's own number, which costs half an hour before the first forecast
    /// exists. The footer states that wait by leaving its left slot empty:
    /// nothing was displaced, because there is no comparison to displace
    /// anything.
    pub(crate) previous: Option<PreviousReading>,
    /// Every `(window, reset)` a forecast has already been announced for.
    ///
    /// **Keyed by the reset rather than counted**, so the answer to "have we
    /// said this already" survives the app being open across several windows
    /// and cannot be reset by anything but the window itself turning over. One
    /// notification per window per epoch: the projection is a fact about that
    /// window, and repeating it every half hour until the reset would be the
    /// same sentence four more times.
    ///
    /// A list rather than a field per window, for the reason [`WindowKey`]
    /// exists: the windows are the source's, and this build knows how many of
    /// them there are only after it has read one. Bounded by
    /// [`ANNOUNCED_MEMORY`], oldest first out - a reset that has passed can
    /// never be announced again anyway, so forgetting it costs nothing.
    pub(crate) announced: Vec<(WindowKey, i64)>,
    /// Whether `claude` was on `PATH` at the last read. Asked off the render
    /// thread beside the read, because it decides whether "not signed in" is a
    /// panel or nothing at all - see [`limits_footer_has_something_to_say`].
    pub(crate) cli_installed: bool,
    /// Projections that are still standing although the latest read did not
    /// repeat them. See [`HeldForecast`].
    pub(crate) held: Vec<HeldForecast>,
    /// The last few hours of successful readings, oldest first. The slope and
    /// the fuse both measure against it; see [`anchor_in`] and [`burn_check`].
    pub(crate) history: Vec<PreviousReading>,
    /// A short window burning right now. See [`BurnAlarm`].
    pub(crate) burn: Option<BurnAlarm>,
}

/// A forecast that keeps its place until two reads in a row have disagreed
/// with it, or until its window resets.
///
/// **A projection that vanished on the next read was a notification with
/// nothing behind it**. The ordinary sequence was: the forecast
/// fires while the window is in the background, the person comes back, the
/// focus read recomputes the slope, and a noisy slope says "survives" - so
/// the footer said nothing about what the notification had just said. One
/// read that disagrees is noise as often as it is news; two in a row is the
/// trend having changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HeldForecast {
    pub(crate) window: WindowKey,
    /// The reset the projection was about. A different reset is a different
    /// window, and the held projection says nothing about it.
    pub(crate) resets_at: i64,
    /// How early the window was projected to run out, as last measured.
    pub(crate) seconds: i64,
    /// Reads in a row that did not repeat the projection.
    pub(crate) misses: u8,
}

/// How many disagreeing reads in a row take a held forecast down.
const HELD_FORECAST_MISSES: u8 = 2;

/// The longest window the footer projects to its reset. See [`forecast`].
const FORECAST_MAX_WINDOW_MINUTES: i64 = 24 * 60;

/// The shortest interval a rate may be measured over.
///
/// **A slope over five minutes is mostly noise.** The server states whole
/// per cent, so a window that moved one point in five minutes projects twelve
/// points an hour, and one that did not move projects nothing - on the same
/// underlying pace. A focus read five minutes after a scheduled one used to
/// become the other end of the slope, which is how a forecast could fire and
/// then contradict itself the moment the person came back to look.
const MIN_RATE_SPAN: i64 = 15 * 60;

/// How many `(window, reset)` marks are kept. Two windows per vendor and a
/// handful of resets is the whole of it; the bound is here so a long-running
/// app cannot grow this without limit.
const ANNOUNCED_MEMORY: usize = 8;

/// One vendor's row: what it said, and the memory the row keeps about it.
///
/// The memory is ours rather than the vendor's - which window was on screen
/// last, and the reading before this one - and it is per vendor for the same
/// reason the rows are: two accounts' windows have nothing to say about each
/// other. It travels with the reading so that a row cannot be drawn with
/// somebody else's memory, which is what would happen the moment the footer
/// drew two rows out of one poller's fields.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VendorRowState {
    pub(crate) reading: VendorLimits,
    /// The window this row showed last, by length. See [`choose_limit`].
    pub(crate) shown: Option<WindowKey>,
    /// The reading before this one, so the forecast has something to subtract.
    pub(crate) previous: Option<PreviousReading>,
}

/// One earlier reading, kept only so the next one has something to subtract.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PreviousReading {
    /// Unix **seconds**, unlike [`ClaudeLimits::last_read_at`], because
    /// everything this takes part in is arithmetic against `resets_at`, which
    /// the server states in seconds.
    pub(crate) at: i64,
    /// What each window read then, keyed by [`WindowKey`]. A window the older
    /// reading did not name simply has no entry, and the slope for it is the
    /// absence of a measurement rather than a zero.
    pub(crate) windows: Vec<(WindowKey, f32)>,
}

impl PreviousReading {
    /// The earlier utilisation of **this** window, paired by length.
    ///
    /// A window with no stated length pairs with the other unnamed one, which
    /// is right for a source that keeps its windows in a stable order and
    /// wrong for nothing we have seen: both real sources state every length
    /// they use.
    fn utilization(&self, window: WindowKey) -> Option<f32> {
        self.windows
            .iter()
            .find(|(key, _)| *key == window)
            .map(|(_, utilization)| *utilization)
    }
}

/// What the two readings say about whether this window survives to its reset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Forecast {
    /// Two readings have not been taken yet, or the window has not moved
    /// between them. **Not a promise that all is well** - it is the absence of
    /// a measurement, and the footer says so in words.
    NoRate,
    /// The newest reading is too old to project from. The footer says when it
    /// was taken rather than pretending the number is current.
    Stale,
    /// At this rate the window outlasts its own reset.
    Survives,
    /// At this rate the window is spent this many seconds before it resets.
    Early { seconds: i64 },
}

/// How old a reading may be before the row stops standing behind it, as a
/// fraction of the window it is about: a fifth.
///
/// **The age of a reading matters in proportion to how fast its window turns.**
/// A fixed hour was right for the five-hour window it was written for and wrong
/// in both directions elsewhere - it calls a weekly figure stale after one
/// missed poll, when a week moves by half a point in that time, and it would
/// call a ninety-minute window fresh after most of it had gone. A fifth of the
/// window reproduces the old hour exactly for Claude's session window, which is
/// the case it was calibrated on.
const STALE_AT_FRACTION_OF_WINDOW: i64 = 5;

/// Never sooner than one poll, so a source we ask cannot be stale between two
/// successful reads of it.
const STALE_NOT_BEFORE: i64 = LIMITS_POLL_INTERVAL.as_secs() as i64;

/// And for a window whose length nobody stated, the rule the fraction replaced:
/// two poll intervals. One missed read is a laptop that slept or a network that
/// blinked; two in a row is a source that has stopped answering.
const STALE_WITHOUT_A_LENGTH: i64 = 2 * 30 * 60;

/// How old a reading of **this** window may be before it is a record rather
/// than a reading, in seconds.
fn stale_after(window_minutes: Option<i64>) -> i64 {
    match window_minutes {
        Some(minutes) if minutes > 0 => {
            (minutes * 60 / STALE_AT_FRACTION_OF_WINDOW).max(STALE_NOT_BEFORE)
        }
        _ => STALE_WITHOUT_A_LENGTH,
    }
}

/// When the window will be spent, against when it resets.
///
/// The slope is `(utilization now - utilization then) / (seconds between)`, from
/// two readings of the vendor's own figure. A window going down - the reset
/// happened between the readings - is no rate rather than a negative one: it
/// is a new window, and the old one's slope says nothing about it.
pub(crate) fn forecast(
    now_utilization: f32,
    now_secs: i64,
    resets_at: Option<i64>,
    previous: Option<(f32, i64)>,
    observed_at_secs: Option<i64>,
    window_minutes: Option<i64>,
) -> Forecast {
    // **A reading whose moment we do not know is stale by definition.** The
    // check used to be skipped in that case, which made "we cannot say how old
    // this is" the one state that was drawn as confidently current.
    let Some(observed_at) = observed_at_secs else {
        return Forecast::Stale;
    };
    if now_secs.saturating_sub(observed_at) > stale_after(window_minutes) {
        return Forecast::Stale;
    }
    // **A window longer than a day is not projected at all.** A
    // straight line from one active half hour across the rest of a week
    // assumes work around the clock, so it said "runs out early" after almost
    // any busy hour.
    if window_minutes.is_some_and(|m| m > FORECAST_MAX_WINDOW_MINUTES) {
        return Forecast::NoRate;
    }
    let (Some((before, then)), Some(resets_at)) = (previous, resets_at) else {
        return Forecast::NoRate;
    };
    // **Between the two observations, and not up to now.** The climb happened
    // between the readings; dividing it by the time since the older one
    // dilutes the rate for as long as the row stays on screen, and a row read
    // half an hour ago would report half the true rate simply for having been
    // looked at.
    let elapsed = observed_at.saturating_sub(then);
    let climbed = now_utilization - before;
    // Too short to be a rate (see `MIN_RATE_SPAN`), not moving, or moving
    // backwards into a fresh window.
    if elapsed < MIN_RATE_SPAN || climbed <= 0.0 {
        return Forecast::NoRate;
    }
    let per_second = f64::from(climbed) / elapsed as f64;
    let remaining = f64::from(100.0 - now_utilization).max(0.0);
    let seconds_to_full = (remaining / per_second).round() as i64;
    let full_at = now_secs.saturating_add(seconds_to_full);
    if full_at >= resets_at {
        Forecast::Survives
    } else {
        Forecast::Early {
            seconds: resets_at - full_at,
        }
    }
}

/// Which of a vendor's windows binds, given what was shown before.
///
/// The prototype's rule is "the session window when `session - weekly > 5pp`,
/// or when `session >= 80`". Applied without memory it flips the label every
/// time the two values cross, which they do all day. This is the same rule as
/// a Schmitt trigger: the shorter window **enters** at a five-point lead and
/// **leaves** only once the longer one has drawn level, so the values have to
/// move five points before anything on screen changes twice.
///
/// A window at its cap overrides all of it: a limit that is reached is the one
/// the user needs to read about, and of two reached windows the shorter one is
/// the wall you hit first.
///
/// **Written over a slice rather than over two named windows**, which changes
/// nothing for a vendor that states two - both of ours do - and makes the rule
/// say what it always meant. The asymmetry is the point of it: 90% of five
/// hours is half an hour of life and 90% of a week is a day of slack, so the
/// shorter window is the privileged one, and it is privileged *by its length*
/// rather than by its name. Windows arrive longest first, and each shorter one
/// in turn may take the row from the incumbent.
///
/// **A window projected to run out before its reset is the binding one**
/// is second only to a window already reached. Settings says the row
/// shows "the binding limit", and a week that will be spent a day early binds
/// harder than a session at 88% that will make it. Without this, a forecast
/// about the window *not* on the meter had nowhere to be seen, which is how a
/// notification could arrive about something the footer never showed.
/// `early` names those windows; of several, the shortest wins.
pub(crate) fn choose_limit(
    previous: Option<WindowKey>,
    limits: &[Limit],
    early: &[WindowKey],
) -> Option<Limit> {
    if limits.is_empty() {
        return None;
    }
    // Longest first, so the fold walks from the most forgiving window to the
    // most binding. A window with no stated length sorts last: it cannot claim
    // to be the short one on evidence nobody gave us.
    let mut ordered: Vec<Limit> = limits.to_vec();
    ordered.sort_by(|a, b| {
        b.window_minutes
            .unwrap_or(i64::MAX)
            .cmp(&a.window_minutes.unwrap_or(i64::MAX))
    });

    // A reached window wins outright, and the shortest of them wins among
    // themselves.
    if let Some(reached) = ordered
        .iter()
        .rev()
        .find(|limit| limit.utilization >= REACHED_AT)
    {
        return Some(*reached);
    }

    // Then a window that will not last to its reset, the shortest first.
    if let Some(projected) = ordered
        .iter()
        .rev()
        .find(|limit| early.contains(&limit.window_minutes))
    {
        return Some(*projected);
    }

    // The plain rule, with no memory: each shorter window takes the row when
    // it leads the incumbent by more than the band, or when it is nearly full
    // on its own account.
    let mut winner = ordered[0];
    for limit in &ordered[1..] {
        let lead = limit.utilization - winner.utilization;
        if lead > SHORTER_TAKES_OVER_AT || limit.utilization >= SHORTER_LATCHES_AT {
            winner = *limit;
        }
    }

    // And the memory. The incumbent is the window that was on screen, if it is
    // still one of the vendor's own.
    let Some(incumbent) = previous.and_then(|key| ordered.iter().find(|l| l.window_minutes == key))
    else {
        return Some(winner);
    };
    if incumbent.window_minutes == winner.window_minutes {
        return Some(winner);
    }
    let winner_is_shorter =
        winner.window_minutes.unwrap_or(i64::MAX) < incumbent.window_minutes.unwrap_or(i64::MAX);
    if winner_is_shorter {
        // Taking the row *for* a shorter window is already banded by the rule
        // above, so it happens as soon as the rule says so.
        Some(winner)
    } else if incumbent.utilization - winner.utilization <= 0.0 {
        // Giving it back to a longer one waits for the incumbent's lead to be
        // gone entirely. That band is the whole of the hysteresis.
        Some(winner)
    } else {
        Some(*incumbent)
    }
}

/// How full the meter is, and therefore what colour it is **and what word
/// stands beside it**. The two are one decision, which is why they are one
/// type: a band that could be coloured without a word is exactly the defect
/// the meter used to have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    /// Below the first threshold. No colour, no word.
    Quiet,
    NearlyOut,
    Reached,
}

impl Level {
    fn of(utilization: f32) -> Self {
        if utilization >= REACHED_AT {
            Level::Reached
        } else if utilization >= NEARLY_OUT_AT {
            Level::NearlyOut
        } else {
            Level::Quiet
        }
    }

    /// The word on screen, or `None` for the band that says nothing.
    ///
    /// It is deliberately not part of the label. `Claude weekly limit reached`
    /// put the state inside the row's *name*, so the one word that has to be
    /// legible without hue was drawn in the label's own muted tone and moved
    /// about as the label changed. It stands on its own now and takes
    /// [`Level::color`], which is what makes `nearly out` stop being drawn in
    /// the error tone.
    fn word(self) -> Option<&'static str> {
        match self {
            Level::Quiet => None,
            Level::NearlyOut => Some("nearly out"),
            Level::Reached => Some("reached"),
        }
    }

    /// The theme's own roles rather than the design's hexes, for the reason
    /// this file has always given: the app has themes and a hot-reload
    /// watcher, and a footer that ignores them reads as a foreign panel pasted
    /// into the window. `dim` for the quiet band is a correction -
    /// see [`NEARLY_OUT_AT`] for why it was the accent and must not be again.
    fn color(self, ui: crate::theme::UiColors) -> Hsla {
        match self {
            Level::Quiet => ui.dim,
            Level::NearlyOut => ui.vc_modified,
            Level::Reached => ui.agent_error,
        }
    }
}

/// Everything the footer prints, worked out before any of it is drawn.
#[derive(Clone, Debug, PartialEq)]
struct FooterCopy {
    /// Row 1, left: which limit this is.
    label: String,
    /// Row 1, right: the percentage, or an em dash.
    value: String,
    /// Row 1, beside the value: a state that is not a band - today only
    /// `not signed in`. Drawn where the band's word goes, in the neutral ink,
    /// because it is a fact about the account rather than about a quantity.
    note: Option<&'static str>,
    /// Whether the row draws a meter at all. **A meter with no quantity
    /// promises one**: a read that failed keeps its bare track, because there
    /// was a reading and it went away, while an account that has never been
    /// read has no track to keep.
    meter: bool,
    /// Row 2: how full the meter is, `0.0..=1.0`. `None` leaves the track bare.
    fill: Option<f32>,
    /// The colour band the meter is in, and therefore the word beside it.
    level: Level,
    /// Row 3, right: when the window resets. A clock when the left slot holds
    /// a forecast - "out 40m early" is an offset from that moment, so the
    /// moment has to be one the reader can check - and a span otherwise.
    detail: String,
    /// Row 3, left: the forecast, and only the forecast.
    ///
    /// **One meaning for the slot**: it is the one text in the
    /// footer about the future, and empty means there is no projection to
    /// make. It used to carry whatever the right slot had lost, including a
    /// clock when the reading was stale; the freshness of a reading now lives
    /// in the origin line under the block, which states it in every state.
    detail_aside: String,
    /// Row 4, left and right: the other limit. The right side carries the
    /// other window's own forecast in place of its reset when it has one.
    other_label: String,
    other_value: String,
    /// What the forecast said, so the row and the affordance beside it cannot
    /// disagree about whether this reading is worth anything.
    outlook: Option<Forecast>,
    /// Whether the number beside the meter is old enough that the row no longer
    /// states it as the present.
    ///
    /// **A stale row may warn and may not reassure**, which is not a
    /// compromise: inside one window utilisation only ever rises, so an old
    /// reading is a **floor** under the true one. `nearly out` an hour ago is
    /// still at least `nearly out`; `18%` an hour ago is no promise of anything
    /// at all. So the coloured bands keep their colour and their word, and the
    /// number itself stops being asserted - it greys, and the origin line says
    /// when it was taken.
    stale: bool,
}

/// What the forecast says about one window, with the held projection filling
/// in for a read that did not repeat it.
///
/// Staleness is never overridden: a held projection is kept *because* two
/// fresh reads have not yet contradicted it, and a reading too old to project
/// from is not a fresh read of anything.
fn outlook_of(
    window: &Limit,
    vendor: &VendorLimits,
    previous: Option<&PreviousReading>,
    held: &[HeldForecast],
    now: i64,
) -> Forecast {
    let fresh = forecast(
        window.utilization,
        now,
        window.resets_at,
        previous.and_then(|p| p.utilization(window.window_minutes).map(|u| (u, p.at))),
        vendor.observed_at,
        window.window_minutes,
    );
    match fresh {
        Forecast::Early { .. } | Forecast::Stale => fresh,
        Forecast::NoRate | Forecast::Survives => held
            .iter()
            .find(|h| h.window == window.window_minutes && Some(h.resets_at) == window.resets_at)
            .map_or(fresh, |h| Forecast::Early { seconds: h.seconds }),
    }
}

/// The windows projected to run out before they reset, for [`choose_limit`].
fn early_windows(
    vendor: &VendorLimits,
    previous: Option<&PreviousReading>,
    held: &[HeldForecast],
    now: i64,
) -> Vec<WindowKey> {
    vendor
        .current(now)
        .iter()
        .filter(|window| {
            matches!(
                outlook_of(window, vendor, previous, held, now),
                Forecast::Early { .. }
            )
        })
        .map(|window| window.window_minutes)
        .collect()
}

/// Build the copy for one vendor's row, `now` being Unix seconds.
///
/// **Takes the normalised reading and our own bookkeeping, and nothing else.**
/// It used to take `&ClaudeLimits`, which is the state of one poller: the two
/// windows by name, the reasons that poller can fail with, and the label
/// "Claude" written into the degraded line. Every one of those is a fact about
/// where the numbers came from, and none of them is a fact about what the row
/// says.
///
/// **Why a read failed is not said here.** It belongs to the origin line (see
/// [`origin_line`]), which is where the one action that answers it lives.
fn footer_copy(
    vendor: &VendorLimits,
    previous: Option<&PreviousReading>,
    held: &[HeldForecast],
    shown: Option<WindowKey>,
    now: i64,
) -> FooterCopy {
    let vendor_name = vendor.vendor.name();
    let unavailable = |detail: String| FooterCopy {
        label: format!("{vendor_name} limits unavailable"),
        value: "\u{2014}".to_string(),
        note: None,
        meter: true,
        fill: None,
        level: Level::Quiet,
        detail,
        detail_aside: String::new(),
        outlook: None,
        other_label: String::new(),
        other_value: String::new(),
        stale: false,
    };

    match &vendor.reading {
        // Read, and the account has no plan window at all. The label never
        // asserts a limit the line under it denies, so it does not say
        // "unavailable" - nothing failed - and there is no meter, because there
        // is no quantity.
        LimitsReading::NoPlanLimit => FooterCopy {
            label: format!("{vendor_name} \u{2014} no plan limit"),
            value: String::new(),
            note: None,
            meter: true,
            fill: None,
            level: Level::Quiet,
            detail: "billed by the key \u{2014} no plan limit to read".to_string(),
            detail_aside: String::new(),
            outlook: None,
            other_label: String::new(),
            other_value: String::new(),
            stale: false,
        },
        // Never read, rather than read and lost: one line, no meter, and the
        // origin line under it says why there is no number and offers the way
        // in.
        LimitsReading::Unreadable(LimitsError::NotSignedIn) => FooterCopy {
            label: format!("{vendor_name} limit"),
            value: String::new(),
            note: Some("not signed in"),
            meter: false,
            ..unavailable(String::new())
        },
        LimitsReading::Unreadable(_) => unavailable(String::new()),
        LimitsReading::Limits(_) => {
            let current = vendor.current(now);
            // Answered, and nothing it said is about the present: every window
            // it named has already started over. Not a failure and not a zero -
            // for a source we overhear it is what a Monday morning looks like -
            // so the row draws no meter and the origin line says when the
            // vendor last spoke.
            if current.is_empty() {
                return FooterCopy {
                    label: format!("{vendor_name} limits"),
                    detail: "no current window".to_string(),
                    ..unavailable(String::new())
                };
            }
            let early = early_windows(vendor, previous, held, now);
            let Some(window) = choose_limit(shown, &current, &early) else {
                return unavailable("partial reply".to_string());
            };
            let other = current
                .iter()
                .find(|limit| limit.window_minutes != window.window_minutes)
                .copied();
            // One number decides all three, and it is the one on screen.
            // The percentage is rounded for display, so the band has to be
            // read off the same rounded value: taken from the raw float, a
            // window at 99.6 would render "100%" beside `nearly out`, and one
            // at 84.7 would render "85%" in silence while Settings -> Limits
            // states the warning begins at 85%. Both are the row disagreeing
            // with itself in the one place a reader can check it. What the row
            // says is what the row is coloured by.
            let shown_percent = window.utilization.round();
            let outlook = outlook_of(&window, vendor, previous, held, now);
            FooterCopy {
                // The state word is no longer glued onto the name - see
                // `Level::word`.
                label: limit_label(Some(vendor_name), window.window_minutes),
                value: format!("{}%", shown_percent as i64),
                note: None,
                meter: true,
                fill: Some(
                    Limit {
                        utilization: shown_percent,
                        ..window
                    }
                    .consumed_fraction(),
                ),
                level: Level::of(shown_percent),
                detail: match (&outlook, window.resets_at) {
                    // Beside a forecast the reset is a clock: "out 40m early"
                    // is measured back from it, so it has to be a moment the
                    // reader can hold the offset against.
                    (Forecast::Early { .. }, Some(at)) if at > now => {
                        format!("resets {}", clock_when(at, now))
                    }
                    _ => match resets_in(&window, now) {
                        Some(when) => format!("resets in {when}"),
                        // No reset time is not a failure: the window is still
                        // read, and the origin line already says when.
                        None => String::new(),
                    },
                },
                detail_aside: match &outlook {
                    Forecast::Early { seconds } => {
                        format!("out {} early", format_span(*seconds))
                    }
                    Forecast::NoRate | Forecast::Survives | Forecast::Stale => String::new(),
                },
                outlook: Some(outlook),
                other_label: match other {
                    Some(other) => limit_label(None, other.window_minutes),
                    None => String::new(),
                },
                other_value: match other {
                    Some(other) => {
                        let percent = other.utilization.round() as i64;
                        // A second window that will not last says so instead
                        // of when it resets: the reset of the window that
                        // binds is already on the line above.
                        match (
                            outlook_of(&other, vendor, previous, held, now),
                            resets_in(&other, now),
                        ) {
                            (Forecast::Early { seconds }, _) => {
                                format!("{percent}% \u{b7} out {} early", format_span(seconds))
                            }
                            (_, Some(when)) => format!("{percent}% \u{b7} {when}"),
                            (_, None) => format!("{percent}%"),
                        }
                    }
                    None => String::new(),
                },
                stale: matches!(outlook, Forecast::Stale),
            }
        }
    }
}

/// The status half of row 3: where the numbers came from, and the one thing
/// to do about them.
///
/// **It stands in every state.** A reason and a `retry now` used to
/// appear only once something had failed or gone stale, which made the one
/// control in the footer a thing that came and went with the age of a
/// reading, and a control gated by a continuous quantity may not do that.
///
/// **It shares row 3 rather than taking a row of its own.** An earlier design
/// drew it as a fifth line under a rule; in use the block grew by a
/// line the rail's list paid for, and the row that says when the window resets
/// already had an empty left slot on every calm day. So the left slot is the
/// forecast when there is one and the moment of the read otherwise, and the
/// action sits at the row's right end.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OriginLine {
    /// Left: freshness, or what went wrong.
    pub(crate) text: String,
    /// What pressing the line does, if anything.
    pub(crate) action: OriginAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginAction {
    /// A `↻` at the row's right end reads again. It stays while a read is
    /// in flight - the press is simply ignored then - so the row does not
    /// shift under the pointer.
    Refresh,
    /// A right-hand `sign in`, and the whole line presses it. Reading again
    /// cannot mend a missing or dead login.
    SignIn,
    /// A right-hand `retry now`, for the failures a second attempt can fix.
    Retry,
    /// Nothing to do: a read is in flight, or the source is one we overhear.
    None,
}

/// The origin line for a vendor's full row.
pub(crate) fn origin_line(vendor: &VendorLimits, reading_in_flight: bool) -> OriginLine {
    let stamp = || {
        vendor
            .observed_at
            .map(|at| clock_hhmm(at as u64 * 1000))
            .unwrap_or_default()
    };
    // A source we overhear has nothing to press: its numbers arrive when the
    // agent next runs.
    if vendor.delivery() == Delivery::Passive {
        let text = match vendor.observed_at {
            Some(_) => format!("{} {}", vendor.delivery().age_word(), stamp()),
            None => String::new(),
        };
        return OriginLine {
            text,
            action: OriginAction::None,
        };
    }
    // **The numbers above stay while a read is in flight**: the last reading
    // is still the truth until one replaces it, and a block that emptied for
    // the length of a request would be wrong in both directions.
    if reading_in_flight {
        return OriginLine {
            text: "reading\u{2026}".to_string(),
            action: OriginAction::Refresh,
        };
    }
    match &vendor.reading {
        LimitsReading::Unreadable(LimitsError::NotSignedIn) => OriginLine {
            text: "nothing read yet".to_string(),
            action: OriginAction::SignIn,
        },
        LimitsReading::Unreadable(err @ LimitsError::ExpiredLogin) => OriginLine {
            text: err.reason().to_string(),
            action: OriginAction::SignIn,
        },
        LimitsReading::Unreadable(err) => OriginLine {
            text: err.reason().to_string(),
            action: OriginAction::Retry,
        },
        LimitsReading::Limits(_) | LimitsReading::NoPlanLimit => OriginLine {
            text: match vendor.observed_at {
                Some(_) => format!("read {}", stamp()),
                None => String::new(),
            },
            action: OriginAction::Refresh,
        },
    }
}

/// A moment the reader can check: `HH:MM` inside the next day, and the weekday
/// in front of it beyond that. A weekly reset printed as a bare `09:00` names
/// a time without saying which of seven days it is on.
fn clock_when(at_secs: i64, now_secs: i64) -> String {
    if at_secs.saturating_sub(now_secs) < 24 * 3600 {
        return clock_hhmm((at_secs.max(0) as u64).saturating_mul(1000));
    }
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(at_secs, 0).single() {
        Some(local) => local.format("%a %H:%M").to_string(),
        None => clock_hhmm((at_secs.max(0) as u64).saturating_mul(1000)),
    }
}

/// "38m", "3h 32m", "2d 4h" - or `None` when the source named no reset.
///
/// A reset already in the past cannot reach here any more: a window whose
/// moment has passed is filtered out one step earlier, by
/// [`Limit::is_current`], because the *percentage* beside it is as stale as the
/// time was. This used to be the only place that noticed, which was enough
/// while a poll every half hour was the only source and is not enough for one
/// we overhear.
fn resets_in(limit: &Limit, now: i64) -> Option<String> {
    let at = limit.resets_at?;
    let left = at - now;
    if left <= 0 {
        return None;
    }
    Some(format_span(left))
}

/// A span of seconds in the two coarsest units it has.
fn format_span(seconds: i64) -> String {
    let minutes = seconds / 60;
    if minutes < 1 {
        return "<1m".to_string();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{}h {}m", hours, minutes % 60);
    }
    format!("{}d {}h", hours / 24, hours % 24)
}

/// Why a read is being started, which is the only thing that decides whether
/// it is allowed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LimitsRefresh {
    /// The app just came up.
    Startup,
    /// The scheduled poll.
    Tick,
    /// The window came back to the front.
    Focus,
    /// The user pressed the origin line: `read 14:32 ↻` or `retry now`.
    Manual,
    /// A Claude session in a pane is alive while the last read said there was
    /// no usable login - which is what signing in inside that pane looks like
    /// from here. Asked by the agent-state pass, and floored.
    AgentActivity,
}

/// How often the limits are polled while the app runs.
///
/// The endpoint is undocumented and unbilled, and every request is traffic to
/// somebody else's server sent under the user's own credentials, so the
/// interval is chosen to be the largest one that still keeps the number
/// useful. The windows are five hours and seven days: half an hour moves the
/// five-hour meter by at most ten points and the weekly one by less than one,
/// which is inside what a reader would call "current". It is also what the one
/// other client we could compare against settles on.
pub(crate) const LIMITS_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// How often the poll asks the wall clock whether the interval has passed.
///
/// **The interval is measured on the wall clock, not on a timer**, because a
/// timer's clock does not advance while the machine sleeps: a laptop opened
/// after a night showed the evening's numbers for up to another half hour.
/// Checking once a minute costs a clock read, and a read is only made when the
/// interval has really gone by.
pub(crate) const LIMITS_TICK_CHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// The first read waits this long after launch. Long enough to be behind
/// window creation, session restore and the PTY spawns, short enough that the
/// footer is filled before anyone has finished reading the rail.
pub(crate) const LIMITS_FIRST_READ_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// The floor between two focus-triggered reads while everything is working.
/// Alt-tabbing is not a request for fresh data thirty times a minute.
const FOCUS_FLOOR: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The floor between two presses of the origin line. The press is always
/// there, so an impatient hand could otherwise become a poller of an endpoint
/// that answers 429.
const MANUAL_FLOOR: std::time::Duration = std::time::Duration::from_secs(30);

/// The floor between two reads asked for by a live Claude session while the
/// login is missing or dead. Signing in takes longer than this, and without a
/// login a read never reaches the network - it fails at the credential lookup.
const AGENT_ACTIVITY_FLOOR: std::time::Duration = std::time::Duration::from_secs(60);

/// How far a run of failures pushes that floor out, and where it stops. A
/// failing read is cheap for us and not free for the server, so a machine with
/// no network and a user who tabs about does not turn into a poller.
const FOCUS_BACKOFF_CAP: std::time::Duration = LIMITS_POLL_INTERVAL;

/// The floor a focus-triggered read has to clear, given a run of failures.
fn focus_floor(consecutive_failures: u32) -> std::time::Duration {
    FOCUS_FLOOR
        .saturating_mul(1u32 << consecutive_failures.min(3))
        .min(FOCUS_BACKOFF_CAP)
}

/// Turn a reading into the thing the next one subtracts from.
fn previous_reading_of(reading: &VendorLimits) -> PreviousReading {
    PreviousReading {
        at: reading.observed_at.unwrap_or_default(),
        windows: match &reading.reading {
            LimitsReading::Limits(limits) => limits
                .iter()
                .map(|limit| (limit.window_minutes, limit.utilization))
                .collect(),
            LimitsReading::NoPlanLimit | LimitsReading::Unreadable(_) => Vec::new(),
        },
    }
}

/// Whether the reading being replaced should become the other end of the
/// slope.
///
/// Only when it is at least [`MIN_RATE_SPAN`] older than its replacement, or
/// when there is nothing to subtract from yet. Otherwise the older anchor
/// stays, which only ever lengthens the interval the rate is measured over.
fn replaces_anchor(anchor: Option<&PreviousReading>, outgoing_at: i64, incoming_at: i64) -> bool {
    anchor.is_none() || incoming_at.saturating_sub(outgoing_at) >= MIN_RATE_SPAN
}

/// Carry the held forecasts across one successful read.
///
/// Every window the fresh read projects early is held anew, with its misses
/// reset. A held one the read did not repeat takes a miss and falls after
/// [`HELD_FORECAST_MISSES`]; one whose window has reset, or is no longer in the
/// reading, goes at once - it was about a window that is not there any more.
fn carry_held(
    held: &[HeldForecast],
    reading: &VendorLimits,
    previous: Option<&PreviousReading>,
    now: i64,
) -> Vec<HeldForecast> {
    let current = reading.current(now);
    let mut next: Vec<HeldForecast> = Vec::new();
    for window in &current {
        let Some(resets_at) = window.resets_at else {
            continue;
        };
        if let Forecast::Early { seconds } = outlook_of(window, reading, previous, &[], now) {
            next.push(HeldForecast {
                window: window.window_minutes,
                resets_at,
                seconds,
                misses: 0,
            });
        }
    }
    for old in held {
        if next.iter().any(|h| h.window == old.window) {
            continue;
        }
        let still_that_window = current
            .iter()
            .any(|w| w.window_minutes == old.window && w.resets_at == Some(old.resets_at));
        let misses = old.misses.saturating_add(1);
        if still_that_window && old.resets_at > now && misses < HELD_FORECAST_MISSES {
            next.push(HeldForecast { misses, ..*old });
        }
    }
    next
}

/// The fuse: a short window being spent so fast it will run out within the
/// hour and a half, and before it resets.
///
/// **Why this and not the projection to the reset.** The feature was asked for
/// after one session burned a whole five-hour window in about
/// an hour. The first build answered a different question - "at the pace of the
/// last half hour, is this window spent before it resets?" - and a straight
/// line drawn across a whole window is wrong in the ordinary case: an active
/// half hour on the weekly window projects a week of round-the-clock work, so
/// it fired on calm days about a window at 29% and said nothing useful. The
/// fuse asks the question that was actually put: **is something burning right
/// now**. So it
/// looks only at windows of five hours or less, over the last half hour at
/// most, needs ten points of real movement before it believes the rate, and
/// only speaks when the wall is close.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BurnAlarm {
    pub(crate) window: WindowKey,
    pub(crate) resets_at: i64,
    /// The window's utilisation at the read that raised the alarm.
    pub(crate) utilization: f32,
    /// How many points went in the measured span.
    pub(crate) points: f32,
    /// The span those points went in, in seconds.
    pub(crate) span: i64,
    /// Unix seconds at which the pace reaches the cap.
    pub(crate) runs_out_at: i64,
    /// The session that spent most in the span, once it has been looked up.
    pub(crate) culprit: Option<Culprit>,
}

/// The surface that did most of the spending while the window burned.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Culprit {
    pub(crate) thread_id: u64,
    pub(crate) title: String,
    pub(crate) project: String,
}

/// Windows longer than this are never fused: a burst of work is not a pace
/// that holds for days.
const BURN_WINDOW_MAX_MINUTES: i64 = 5 * 60;
/// How far back the fuse measures. Long enough to see a burst, short enough
/// that a burst is not averaged away by a quiet hour before it.
const BURN_LOOKBACK: i64 = 30 * 60;
/// The shortest span a burn is measured over.
const BURN_MIN_SPAN: i64 = 5 * 60;
/// Movement the fuse needs before it believes the rate. The server states
/// whole per cent, so a couple of points is rounding; ten is work.
const BURN_MIN_POINTS: f32 = 10.0;
/// How close the wall has to be. A window that will run out in three hours at
/// this pace is busy; one that runs out within the hour and a half is the
/// case the fuse exists for.
const BURN_HORIZON: i64 = 90 * 60;
/// A climb this big between two reads makes the poll quicken, before the fuse
/// has enough to say anything.
const BURN_WATCH_POINTS: f32 = 5.0;
/// How long readings are kept for the rate and the fuse to measure against.
const HISTORY_KEEP: i64 = 3 * 3600;
/// And how many, whatever their age.
const HISTORY_MAX: usize = 64;

/// The poll's cadence while nothing is happening, while a Claude session is
/// working, and while a window is climbing fast. The quick ones are what make
/// the fuse useful: at thirty minutes it could only ever report a window that
/// was already half gone.
const POLL_WHILE_WORKING: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const POLL_WHILE_BURNING: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Whether a short window is burning, measured against the readings before
/// this one.
///
/// `history` is older readings, oldest first, **not** including `vendor`. The
/// reference is the oldest one inside [`BURN_LOOKBACK`] that is at least
/// [`BURN_MIN_SPAN`] back, so the rate is over as long a stretch as the last
/// half hour allows. Of two burning windows the shorter one is named.
pub(crate) fn burn_check(vendor: &VendorLimits, history: &[PreviousReading]) -> Option<BurnAlarm> {
    let observed_at = vendor.observed_at?;
    let mut windows = vendor.current(observed_at);
    windows.sort_by_key(|w| w.window_minutes.unwrap_or(i64::MAX));
    windows.into_iter().find_map(|window| {
        let minutes = window.window_minutes?;
        if minutes > BURN_WINDOW_MAX_MINUTES || window.utilization >= REACHED_AT {
            return None;
        }
        let resets_at = window.resets_at?;
        // **Only readings from this window's own epoch.** Inside one window
        // utilisation only rises, so a later reading lower than an earlier one
        // is a reset between them, and anything before it belongs to the
        // previous window: measured against it, a burst in a fresh window
        // reads as small or negative and the fuse stays dark exactly when it
        // should not.
        let in_reach: Vec<(i64, f32)> = history
            .iter()
            .filter(|reading| observed_at - reading.at <= BURN_LOOKBACK)
            .filter_map(|reading| {
                reading
                    .utilization(window.window_minutes)
                    .map(|u| (reading.at, u))
            })
            .collect();
        let epoch_start = in_reach
            .windows(2)
            .rposition(|pair| pair[1].1 < pair[0].1)
            .map_or(0, |at| at + 1);
        let (then, before) = in_reach[epoch_start..]
            .iter()
            .copied()
            .find(|(at, u)| observed_at - at >= BURN_MIN_SPAN && *u <= window.utilization)?;
        let points = window.utilization - before;
        if points < BURN_MIN_POINTS {
            return None;
        }
        let span = observed_at - then;
        let per_second = f64::from(points) / span as f64;
        let to_full = (f64::from(100.0 - window.utilization) / per_second).round() as i64;
        let runs_out_at = observed_at.saturating_add(to_full);
        (to_full <= BURN_HORIZON && runs_out_at < resets_at).then_some(BurnAlarm {
            window: window.window_minutes,
            resets_at,
            utilization: window.utilization,
            points,
            span,
            runs_out_at,
            culprit: None,
        })
    })
}

/// The reading the slope subtracts from: the newest one at least
/// [`MIN_RATE_SPAN`] before `now`.
fn anchor_in(history: &[PreviousReading], now: i64) -> Option<PreviousReading> {
    history
        .iter()
        .rev()
        .find(|reading| now - reading.at >= MIN_RATE_SPAN)
        .cloned()
}

/// Add a reading to the history and forget what is too old to matter.
fn remember(history: &mut Vec<PreviousReading>, reading: PreviousReading, now: i64) {
    history.push(reading);
    history.retain(|r| now - r.at <= HISTORY_KEEP);
    let overflow = history.len().saturating_sub(HISTORY_MAX);
    history.drain(..overflow);
}

/// Whether a short window climbed fast enough between the last two reads to
/// watch it closely.
fn climbing_fast(history: &[PreviousReading]) -> bool {
    let [.., before, last] = history else {
        return false;
    };
    // **A pace, not a difference.** Five points over a half-hour idle poll, or
    // over a night the laptop slept, is a calm window; the same five points in
    // ten minutes is the start of a burst. Scaled to the ten-minute cadence of
    // a working session.
    let minutes = (last.at - before.at) as f32 / 60.0;
    if minutes <= 0.0 {
        return false;
    }
    last.windows.iter().any(|(window, now)| {
        window.is_some_and(|m| m <= BURN_WINDOW_MAX_MINUTES)
            && before
                .utilization(*window)
                .is_some_and(|then| (now - then) / minutes * 10.0 >= BURN_WATCH_POINTS)
    })
}

/// Which surface spent most since `since`, from each one's own transcript.
///
/// Blocking I/O: call inside `smol::unblock`. Each entry is
/// `(thread_id, title, project, transcript)`. `None` when nothing in this app
/// spent anything in the span - the burn is then somewhere else, a terminal
/// outside Splitlane, and naming a pane would be naming the wrong thing.
fn find_culprit(
    surfaces: Vec<(u64, String, String, std::path::PathBuf)>,
    since: i64,
) -> Option<Culprit> {
    surfaces
        .into_iter()
        .filter_map(|(thread_id, title, project, path)| {
            let facts = crate::claude_sessions::read_tail_facts(&path)?;
            let dollars: f64 = facts
                .spend
                .iter()
                .filter(|sample| sample.at >= since)
                .map(|sample| sample.dollars)
                .sum();
            (dollars > 0.0).then_some((dollars, thread_id, title, project))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, thread_id, title, project)| Culprit {
            thread_id,
            title,
            project,
        })
}

/// The notification's title and body.
///
/// The wording: **what is happening, the numbers to check it by, and
/// who**. The title names the window - a person has two - and says the event
/// rather than a forecast. The body gives the points and the span they went
/// in, the moment the pace reaches the wall against the moment the window
/// comes back, and the session doing it.
fn burn_notification_text(vendor: Vendor, burn: &BurnAlarm, now: i64) -> (String, String) {
    // **"Busiest here", not "mostly".** Dollars from a transcript and points of
    // a window are different units, and a terminal outside this app spends
    // from the same window unseen - so the claim is the ranking inside
    // Splitlane, which is the one thing measured.
    let who = match &burn.culprit {
        Some(culprit) => format!(
            " Busiest here: \u{ab}{}\u{bb} in {}.",
            culprit.title, culprit.project
        ),
        None => " Not traced to a Splitlane session.".to_string(),
    };
    (
        format!(
            "{} is burning fast",
            limit_label(Some(vendor.name()), burn.window)
        ),
        format!(
            "{}% gone in the last {} \u{2014} at this pace it runs out at {} (resets {}).{who}",
            burn.points.round() as i64,
            format_span(burn.span),
            clock_when(burn.runs_out_at, now),
            clock_when(burn.resets_at, now),
        ),
    )
}

impl SplitlaneApp {
    /// Re-read the limits, if this reason is allowed to.
    ///
    /// Nothing here touches the network: the read itself is one
    /// [`crate::claude_usage::poll`] on a background thread, and only the
    /// result comes back to the GPUI thread.
    pub(crate) fn refresh_claude_limits(&mut self, why: LimitsRefresh, cx: &mut Context<Self>) {
        if self.claude_limits.reading {
            return;
        }
        let floor = match why {
            LimitsRefresh::Startup | LimitsRefresh::Tick => None,
            LimitsRefresh::Focus => Some(focus_floor(self.claude_limits.consecutive_failures)),
            LimitsRefresh::Manual => Some(MANUAL_FLOOR),
            LimitsRefresh::AgentActivity => Some(AGENT_ACTIVITY_FLOOR),
        };
        if let (Some(floor), Some(last)) = (floor, self.claude_limits.last_read_at) {
            let since = now_unix_millis().saturating_sub(last);
            if since < floor.as_millis() as u64 {
                return;
            }
        }

        self.claude_limits.reading = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let (result, cli_installed) = smol::unblock(|| {
                (
                    crate::claude_usage::poll(),
                    crate::agent_launcher::TerminalAgent::ClaudeCode.is_installed(),
                )
            })
            .await;
            let _ = this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                app.apply_claude_limits(result, cli_installed, cx);
            });
        })
        .detach();
    }

    /// Whether the last successful reading is recent enough for the fuse to
    /// stand on.
    fn claude_limits_fresh(&self, now: i64) -> bool {
        self.claude_limits
            .history
            .last()
            .is_some_and(|reading| now - reading.at <= BURN_LOOKBACK)
    }

    /// Whether the scheduled poll is due, by the wall clock. See
    /// [`LIMITS_TICK_CHECK`].
    ///
    /// **The cadence follows what is happening.** Half an hour
    /// while nothing is; ten minutes while a Claude session is working, since
    /// that is the only time the numbers move; five while a short window is
    /// climbing fast or the fuse is lit, which is the stretch a person needs to
    /// hear about within minutes rather than after half the window has gone.
    pub(crate) fn claude_limits_tick_due(&self) -> bool {
        let interval =
            if self.claude_limits.burn.is_some() || climbing_fast(&self.claude_limits.history) {
                POLL_WHILE_BURNING
            } else if self.workspaces.iter().any(|container| {
                container.threads.iter().any(|thread| {
                    thread.terminal_agent == Some(crate::agent_launcher::TerminalAgent::ClaudeCode)
                        && thread.status == crate::project::ThreadStatus::Thinking
                })
            }) {
                POLL_WHILE_WORKING
            } else {
                LIMITS_POLL_INTERVAL
            };
        // **Failures slow it down whatever it was doing.** A quick cadence is
        // earned by numbers moving; a rate limit or a dead network earns the
        // same back-off a focus read gets, so an outage cannot keep the fast
        // poll running on the strength of an old reading.
        let interval = match self.claude_limits.consecutive_failures {
            0 => interval,
            failures => interval.max(focus_floor(failures)),
        };
        match self.claude_limits.last_read_at {
            Some(last) => now_unix_millis().saturating_sub(last) >= interval.as_millis() as u64,
            None => true,
        }
    }

    /// Ask for a read after a pass of the agent-state detector, when a live
    /// Claude session may have just mended the login.
    ///
    /// Two failures, two signals. **Not signed in** is answered by any Claude
    /// session being on screen at all: signing in happens inside one, and a
    /// read without credentials fails at the lookup, so asking once a minute
    /// costs no request. **An expired login** is answered by a session that is
    /// working: the CLI refreshes its own token when it runs, and a read with a
    /// dead token is a real request, so it waits for evidence rather than a
    /// pane.
    pub(crate) fn refresh_claude_limits_after_agent_pass(&mut self, cx: &mut Context<Self>) {
        let needs_work = match self.claude_limits.state.as_ref() {
            Some(Err(LimitsError::NotSignedIn)) => false,
            Some(Err(LimitsError::ExpiredLogin)) => true,
            _ => return,
        };
        let live_claude = self.workspaces.iter().any(|container| {
            container.threads.iter().any(|thread| {
                thread.terminal_agent == Some(crate::agent_launcher::TerminalAgent::ClaudeCode)
                    && (!needs_work || thread.status == crate::project::ThreadStatus::Thinking)
            })
        });
        if live_claude {
            self.refresh_claude_limits(LimitsRefresh::AgentActivity, cx);
        }
    }

    /// Open a Claude Code pane in the active project, where the CLI asks for
    /// the login itself. The app draws no login form of its own.
    pub(crate) fn sign_in_to_claude(&mut self, cx: &mut Context<Self>) {
        if self.workspaces.is_empty() {
            return;
        }
        self.create_agent_terminal_thread_in(
            self.active_idx,
            crate::agent_launcher::TerminalAgent::ClaudeCode,
            cx,
        );
    }

    /// Fold one reading into the footer's state.
    fn apply_claude_limits(
        &mut self,
        result: Result<UsageSnapshot, LimitsError>,
        cli_installed: bool,
        cx: &mut Context<Self>,
    ) {
        self.claude_limits.reading = false;
        self.claude_limits.cli_installed = cli_installed;
        self.claude_limits.last_read_at = Some(now_unix_millis());
        let now = now_unix_secs();
        match &result {
            Ok(_) => {
                self.claude_limits.consecutive_failures = 0;
                let reading = crate::vendor_limits::from_claude(&result, now);
                // The reading the last run ended on arrives as `previous`;
                // it is the first entry of this run's history.
                if self.claude_limits.history.is_empty()
                    && let Some(restored) = self.claude_limits.previous.clone()
                {
                    remember(&mut self.claude_limits.history, restored, now);
                }
                // The fuse measures against the readings before this one.
                let burn = burn_check(&reading, &self.claude_limits.history);
                self.claude_limits.burn = burn.map(|mut burn| {
                    // The same window still burning keeps the session already
                    // named, until the lookup for this read replaces it.
                    if let Some(was) = &self.claude_limits.burn
                        && was.window == burn.window
                        && was.resets_at == burn.resets_at
                    {
                        burn.culprit = was.culprit.clone();
                    }
                    burn
                });
                // **The slope's other end is the newest reading at least a
                // quarter of an hour back**, not simply the last one: a focus
                // read five minutes after a scheduled one used to become the
                // anchor, and a slope over five minutes is noise.
                if let Some(anchor) = anchor_in(&self.claude_limits.history, now) {
                    self.claude_limits.previous = Some(anchor);
                }
                remember(
                    &mut self.claude_limits.history,
                    previous_reading_of(&reading),
                    now,
                );
                self.claude_limits.held = carry_held(
                    &self.claude_limits.held,
                    &reading,
                    self.claude_limits.previous.as_ref(),
                    now,
                );
                // Which window binds is decided over the reading in its
                // normalised form. A burning window binds hardest of all
                // short of one already reached.
                let mut early = early_windows(
                    &reading,
                    self.claude_limits.previous.as_ref(),
                    &self.claude_limits.held,
                    now,
                );
                if let Some(burn) = &self.claude_limits.burn {
                    early.insert(0, burn.window);
                }
                self.claude_limits.shown =
                    match choose_limit(self.claude_limits.shown, &reading.current(now), &early) {
                        Some(window) => Some(window.window_minutes),
                        // Nothing current to show: keep pointing at whatever
                        // was on screen, so a window that comes back after a
                        // blank read does not also move the row.
                        None => self.claude_limits.shown,
                    };
            }
            Err(err) => {
                self.claude_limits.consecutive_failures =
                    self.claude_limits.consecutive_failures.saturating_add(1);
                log::info!(
                    "claude usage: read failed ({}), attempt {} in a row",
                    err.reason(),
                    self.claude_limits.consecutive_failures
                );
                // A failed read says nothing about the burn. Once the last
                // good reading is older than the span the fuse looks back
                // over, the alarm is about a past that nobody has checked,
                // and it goes out rather than drive the poll and the footer.
                if !self.claude_limits_fresh(now) {
                    self.claude_limits.burn = None;
                }
            }
        }
        let succeeded = result.is_ok();
        self.claude_limits.state = Some(result);
        // Only a new reading can say who is burning: on a failure the span to
        // attribute would slide forward past the burst it measured.
        if succeeded {
            self.name_the_burn(cx);
        }
        cx.notify();
    }

    /// Claude's limits in the shared shape, or `None` when there is nothing
    /// true to say about them yet.
    ///
    /// The one place the poller's own state - two windows by name, five reasons
    /// it can fail with, the moment we asked - becomes a reading like any other
    /// vendor's. Everything downstream of here is written for a vendor rather
    /// than for this one.
    fn claude_vendor_limits(&self) -> Option<VendorLimits> {
        let state = self.claude_limits.state.as_ref()?;
        if !limits_footer_has_something_to_say(Some(state), self.claude_limits.cli_installed) {
            return None;
        }
        // No moment, no reading: the two are written together on every path
        // that produces a state, and inventing `now` for a state that somehow
        // arrived without one would make it the freshest thing on screen.
        let observed_at = self.claude_limits.last_read_at?;
        Some(crate::vendor_limits::from_claude(
            state,
            (observed_at / 1000) as i64,
        ))
    }

    /// While a window burns, find who is burning it, then say so once per
    /// window per reset.
    ///
    /// The lookup reads the tail of every Claude surface's transcript, in every
    /// project and not only the one on screen, off the render thread. It runs on
    /// every read while the fuse is lit, so the name follows the burn if a
    /// different session takes over.
    fn name_the_burn(&mut self, cx: &mut Context<Self>) {
        let Some(burn) = self.claude_limits.burn.clone() else {
            return;
        };
        let Some(since) = self
            .claude_limits
            .history
            .last()
            .map(|reading| reading.at.saturating_sub(burn.span))
        else {
            return;
        };
        let surfaces: Vec<(u64, String, String, std::path::PathBuf)> = self
            .workspaces
            .iter()
            .flat_map(|container| {
                container.threads.iter().filter_map(|thread| {
                    if thread.terminal_agent
                        != Some(crate::agent_launcher::TerminalAgent::ClaudeCode)
                    {
                        return None;
                    }
                    let path = crate::claude_sessions::transcript_path(thread)?;
                    Some((
                        thread.id,
                        thread.title.clone(),
                        container.title.clone(),
                        path,
                    ))
                })
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let culprit = smol::unblock(move || find_culprit(surfaces, since)).await;
            let _ = this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                // The fuse may have gone out, or moved to another window, while
                // the transcripts were being read.
                match app.claude_limits.burn.as_mut() {
                    Some(current)
                        if current.window == burn.window && current.resets_at == burn.resets_at =>
                    {
                        current.culprit = culprit;
                    }
                    _ => return,
                }
                app.announce_burn(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Say, once per window per reset, that a window is burning.
    ///
    /// **The event is the burn, not a threshold and not a projection.**
    /// Crossing 85% is a state and the footer carries it; a line drawn to the
    /// reset was wrong about every window it was asked of (see [`BurnAlarm`]).
    /// What changes what somebody does now is learning that something is eating
    /// the window fast enough to hit the wall within the hour and a half.
    ///
    /// Nothing is said about a limit already **reached**: the CLI's own
    /// requests fail then, and the session that stops is announced by the
    /// notifications next door.
    fn announce_burn(&mut self, cx: &mut Context<Self>) {
        let Some(burn) = self.claude_limits.burn.clone() else {
            return;
        };
        let mark = (burn.window, burn.resets_at);
        if self.claude_limits.announced.contains(&mark) {
            return;
        }
        let (summary, body) = burn_notification_text(Vendor::Claude, &burn, now_unix_secs());
        let announced = crate::agents::notifications::fire_desktop_notification(
            crate::agents::notifications::DesktopNotification::limit_forecast(summary, body),
            &self.cached_config,
            // The limit belongs to the account rather than to any surface, so
            // the question "can you already see this" is about the window: the
            // footer is saying it on screen the whole time.
            crate::agents::notifications::window_active(),
            cx.background_executor().clone(),
        );
        // **The mark records a sentence that was said, not one that was
        // attempted.** Suppressed because the window was in front is untold,
        // and the next read asks again - which, while burning, is five minutes
        // away.
        if !announced {
            return;
        }
        self.claude_limits.announced.push(mark);
        let overflow = self
            .claude_limits
            .announced
            .len()
            .saturating_sub(ANNOUNCED_MEMORY);
        self.claude_limits.announced.drain(..overflow);
    }

    /// Show the session the fuse named.
    pub(crate) fn reveal_burn_culprit(&mut self, thread_id: u64, cx: &mut Context<Self>) {
        let Some(crate::project::AgentsTarget::Thread { ws_idx, thread_idx }) =
            crate::project::find_surface(&self.workspaces, thread_id)
        else {
            self.show_toast("That session has been closed", cx);
            return;
        };
        let _ = self.select_thread(ws_idx, thread_idx, cx);
    }

    /// Re-read Codex's own statement about its limits, off the render thread.
    ///
    /// **A read and not a poll.** Nothing is asked of anybody: the file is
    /// already on disk, written by Codex as it worked, so this is cheap, needs
    /// no credentials and cannot fail in a way worth reporting. That is also
    /// why a failure here is silence rather than a degraded row - a person who
    /// has never run Codex has no Codex row, exactly as a person who has never
    /// signed into Claude has no Claude one.
    ///
    /// It rides the same tick as the polled source because the two answers are
    /// drawn together, not because the interval means anything here.
    pub(crate) fn refresh_codex_limits(&mut self, cx: &mut Context<Self>) {
        if self.codex_limits_reading {
            return;
        }
        self.codex_limits_reading = true;
        cx.spawn(async move |this, cx| {
            let found = smol::unblock(|| {
                crate::codex_sessions::read_account_rate_limits(std::time::Duration::from_secs(
                    crate::vendor_limits::FRESHNESS_HORIZON as u64,
                ))
            })
            .await;
            let _ = this.update(cx, |app: &mut Self, cx: &mut Context<Self>| {
                app.codex_limits_reading = false;
                let Some(reading) = found.as_ref().map(crate::vendor_limits::from_codex) else {
                    app.codex_limits = None;
                    cx.notify();
                    return;
                };
                let now = now_unix_secs();
                let was = app.codex_limits.take();
                // **Only a genuinely newer statement displaces the older one.**
                // Reading the same line again is not a second observation, and
                // letting it become one would leave the slope subtracting a
                // number from itself for as long as the agent stays idle - `no
                // rate yet`, on data that has a rate. And, as for the polled
                // source, only one far enough back to measure a rate over.
                let previous = match (
                    &was,
                    was.as_ref().and_then(|s| s.reading.observed_at),
                    reading.observed_at,
                ) {
                    (Some(was), Some(before), Some(now))
                        if now > before && replaces_anchor(was.previous.as_ref(), before, now) =>
                    {
                        Some(previous_reading_of(&was.reading))
                    }
                    (Some(was), _, _) => was.previous.clone(),
                    (None, _, _) => None,
                };
                let early = early_windows(&reading, previous.as_ref(), &[], now);
                let shown = choose_limit(
                    was.as_ref().and_then(|s| s.shown),
                    &reading.current(now),
                    &early,
                )
                .map(|window| window.window_minutes)
                .or_else(|| was.as_ref().and_then(|s| s.shown));
                app.codex_limits = Some(VendorRowState {
                    reading,
                    shown,
                    previous,
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-read the limits on the user's own say-so.
    pub(crate) fn retry_claude_limits(&mut self, cx: &mut Context<Self>) {
        self.refresh_claude_limits(LimitsRefresh::Manual, cx);
    }

    /// Which vendors have a row, in the order they are drawn.
    ///
    /// **Order is presence and focus, and nothing else** - no hue, no
    /// brightness, no mark. The vendor of the surface the reader is in comes
    /// first and gets the full row, because a full row is not a decoration of
    /// importance: it is more about the vendor they are about to run into,
    /// which is exactly what the focused pane makes relevant.
    ///
    /// Silence is a state here too. A source we overhear with nothing current
    /// to say has **no row** - the person has not run that agent lately, and
    /// nothing is wrong, so nothing is announced. A source we ask keeps its row
    /// in the same situation, because there it means the answer we got is not
    /// about now, which is a fact about the reading rather than about them.
    fn limits_rows(&self, window: &gpui::Window, cx: &gpui::App, now: i64) -> Vec<VendorRowState> {
        let mut rows: Vec<VendorRowState> = Vec::with_capacity(2);
        if let Some(reading) = self.claude_vendor_limits() {
            rows.push(VendorRowState {
                reading,
                shown: self.claude_limits.shown,
                previous: self.claude_limits.previous.clone(),
            });
        }
        if let Some(codex) = self.codex_limits.clone()
            && !(codex.reading.delivery() == Delivery::Passive
                && (codex.reading.nothing_current(now)
                    || codex
                        .reading
                        .age(now)
                        .is_some_and(|age| age > crate::vendor_limits::FRESHNESS_HORIZON)))
        {
            rows.push(codex);
        }
        if let Some(focused) = self.focused_vendor(window, cx)
            && let Some(at) = rows.iter().position(|row| row.reading.vendor == focused)
        {
            rows.swap(0, at);
        }
        rows
    }

    /// The held forecasts that belong to this vendor's row. Only the polled
    /// source holds any: the one we overhear is read afresh on every tick and
    /// states its own moment.
    fn held_for(&self, vendor: Vendor) -> &[HeldForecast] {
        match vendor {
            Vendor::Claude => &self.claude_limits.held,
            Vendor::Codex => &[],
        }
    }

    /// The vendor of the agent in the focused pane, when the focused pane holds
    /// one.
    fn focused_vendor(&self, window: &gpui::Window, cx: &gpui::App) -> Option<Vendor> {
        let crate::app::workspace_ops::FocusedSurface::Agent { ws_idx, thread_id } =
            self.focused_surface(window, cx)?
        else {
            return None;
        };
        let thread = self
            .workspaces
            .get(ws_idx)?
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)?;
        match thread.terminal_agent? {
            crate::agent_launcher::TerminalAgent::ClaudeCode => Some(Vendor::Claude),
            crate::agent_launcher::TerminalAgent::Codex => Some(Vendor::Codex),
            // Fourteen agents whose limits this build cannot read. Focus on one
            // of them reorders nothing, which is the honest answer: we have
            // nothing of theirs to put first.
            _ => None,
        }
    }

    pub(super) fn render_rail_limits_footer(
        &self,
        ui: crate::theme::UiColors,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = now_unix_secs();
        let rows = self.limits_rows(window, cx, now);
        let Some((first, rest)) = rows.split_first() else {
            return div().into_any_element();
        };
        let reading = first.reading.clone();
        let copy = footer_copy(
            &reading,
            first.previous.as_ref(),
            self.held_for(reading.vendor),
            first.shown,
            now,
        );
        let fill_color = copy.level.color(ui);
        let origin = origin_line(
            &reading,
            reading.vendor == Vendor::Claude && self.claude_limits.reading,
        );

        // Built first: the rows below move the copy's strings into the tree.
        // The fuse speaks on the row of the window it is about, and only
        // while that window is the one on the meter.
        let burn = self.claude_limits.burn.clone().filter(|burn| {
            reading.vendor == Vendor::Claude
                && burn.resets_at > now
                && first.shown == Some(burn.window)
                && self.claude_limits_fresh(now)
        });
        let status_row = self.render_limits_status_row(ui, &copy, origin, burn, cx);

        // The panel's frame - background, rule and inset - belongs to the
        // wrapper the rail builds, because the application row shares it: round
        // 8 puts that row *inside* this footer, under a rule, and two blocks
        // with two backgrounds read as two panels.
        div()
            .flex_none()
            // Top inset lives on the wrapper, which is what this comment always
            // claimed: the panel can now be absent, and the application row
            // under it must not end up flush against the rule when it is.
            // The bottom inset stays here - it separates two rows, and there is
            // nothing to separate when this one is gone.
            .pb(tok::space::XL)
            .child(
                // Row 1: which limit, and how much of it is gone.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::SM)
                    .pb(tok::space::SM)
                    // The word and the number are both `flex_none`, so their
                    // intrinsic widths are a floor the label cannot shrink
                    // past. At `RAIL_WIDTH_MIN` the label still absorbs it, so
                    // this is a guard rather than a fix - but the failure it
                    // guards against is text bleeding into the column beside
                    // it, which is worse than a clipped name.
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(tok::text::CAPTION)
                            .text_color(ui.muted)
                            .child(SharedString::from(copy.label)),
                    )
                    // The state word, in the meter's own colour. It sits
                    // between the name and the number because that is where a
                    // reader who cannot see the hue meets it on the way to the
                    // percentage, and it is `flex_none` while the label
                    // truncates, so a narrow rail drops characters from the
                    // name rather than from the one thing carrying the state.
                    .when_some(copy.level.word(), |row, word| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(tok::text::CAPTION)
                                .text_color(fill_color)
                                .child(SharedString::from(word)),
                        )
                    })
                    // A state that is not a band - `not signed in` - in the
                    // neutral ink, in the same place.
                    .when_some(copy.note, |row, note| {
                        row.child(
                            div()
                                .flex_none()
                                .font_family(tok::font::MONO)
                                .text_size(tok::mono::HINT)
                                .text_color(ui.dim)
                                .child(SharedString::from(note)),
                        )
                    })
                    .when(!copy.value.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_none()
                                .font_family(tok::font::MONO)
                                .text_size(tok::mono::ROW)
                                // **Grey is "I do not know", and an old number
                                // is exactly that.** The colour beside it is
                                // untouched - see `FooterCopy::stale` for why a
                                // stale row may still warn - so the two carry
                                // different halves of the same sentence: the
                                // band says how bad it was, the ink says
                                // whether that is still the present.
                                .text_color(if copy.stale { ui.muted } else { ui.text })
                                .child(SharedString::from(copy.value)),
                        )
                    }),
            )
            // Row 2: the meter. An unread limit leaves the track bare rather
            // than drawing a zero-width fill, which would read as "0% used".
            .when(copy.meter, |block| {
                block.child(
                    div()
                        .h(px(METER_HEIGHT))
                        .w_full()
                        .rounded(tok::radius::METER)
                        .bg(ui.border)
                        .when_some(copy.fill, |track, fraction| {
                            track.child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(fraction.max(0.02)))
                                    .rounded(tok::radius::METER)
                                    .bg(fill_color),
                            )
                        }),
                )
            })
            // Row 3: the forecast or the moment of the read on the left, the
            // reset on the right, and the block's one action at the end.
            .child(status_row)
            // Row 4: the other window of the same vendor.
            .children((!copy.other_label.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::SM)
                    .pt(tok::space::XS)
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(ui.muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(copy.other_label)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(SharedString::from(copy.other_value)),
                    )
            }))
            // And one line per other vendor, under it.
            .children(rest.iter().map(|row| {
                render_compact_limits_row(row, self.held_for(row.reading.vendor), ui, now)
            }))
            .into_any_element()
    }

    /// Row 3: the forecast or the status on the left, the reset on the right,
    /// and the block's one action at the end. See [`OriginLine`].
    fn render_limits_status_row(
        &self,
        ui: crate::theme::UiColors,
        copy: &FooterCopy,
        origin: OriginLine,
        burn: Option<BurnAlarm>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The forecast outranks the status for the left slot: it is the one
        // text in the footer about the future, drawn one step above the row by
        // taking a slot rather than a hue. The moment of the read it displaces
        // is still one hover away, on the `↻`.
        let left = if let Some(burn) = burn {
            // The fuse outranks both: it is the one statement here that asks
            // for something now. `burning` takes the warning band's own hue -
            // never the accent - and the name beside it stays in the row's ink.
            let window = limit_label(None, burn.window);
            let now = now_unix_secs();
            let mut tooltip = format!(
                "{}% of the {window} in the last {} \u{2014} at this pace it runs out at {} \
                 (resets {}).",
                burn.points.round() as i64,
                format_span(burn.span),
                clock_when(burn.runs_out_at, now),
                clock_when(burn.resets_at, now),
            );
            let tail = match &burn.culprit {
                Some(culprit) => {
                    tooltip.push_str(&format!(
                        " Busiest here: \u{ab}{}\u{bb} in {}. Click to show it.",
                        culprit.title, culprit.project
                    ));
                    format!(" \u{b7} {}", culprit.title)
                }
                None => format!(
                    " \u{b7} {}% in {}",
                    burn.points.round() as i64,
                    format_span(burn.span)
                ),
            };
            let resting = ui.dim;
            let hovered = ui.text;
            let body = div()
                .id("rail-limits-burn")
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(resting)
                .tooltip(crate::ui_primitives::text_tooltip(tooltip))
                .child(
                    gpui::StyledText::new(SharedString::from(format!("burning{tail}")))
                        .with_highlights([(
                            0.."burning".len(),
                            gpui::HighlightStyle {
                                color: Some(ui.vc_modified),
                                ..Default::default()
                            },
                        )]),
                );
            match burn.culprit.map(|culprit| culprit.thread_id) {
                Some(thread_id) => body
                    .role(Role::Button)
                    .aria_label("Show the session burning the limit")
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.reveal_burn_culprit(thread_id, cx);
                    }))
                    .animated_hover(move |style, delta| {
                        style.text_color(lerp_color(resting, hovered, delta));
                    })
                    .into_any_element(),
                None => body.into_any_element(),
            }
        } else if copy.detail_aside.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(origin.text))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(ui.dim)
                .child(SharedString::from(copy.detail_aside.clone()))
                .into_any_element()
        };
        let row = div()
            .id("rail-limits-status")
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::SM)
            .pt(tok::space::SM)
            .font_family(tok::font::MONO)
            .text_size(tok::mono::HINT)
            .text_color(ui.faint)
            .child(left)
            .when(!copy.detail.is_empty(), |row| {
                row.child(
                    div()
                        .flex_none()
                        .child(SharedString::from(copy.detail.clone())),
                )
            });
        let last_read = self.claude_limits.last_read_at;
        match origin.action {
            // No mark beyond the glyph: the read also happens on focus and on
            // the tick, so the press is an accelerator and never the only way.
            OriginAction::Refresh => {
                let resting = ui.dim;
                let hovered = ui.text;
                let tooltip = match last_read {
                    Some(at) => format!("Read the limits again (last read {})", clock_hhmm(at)),
                    None => "Read the limits now".to_string(),
                };
                row.child(
                    div()
                        .id("rail-limits-refresh")
                        .role(Role::Button)
                        .aria_label("Read the limits again")
                        .flex_none()
                        // A glyph is a small target; the padding widens the
                        // hit area without adding a line.
                        .px(tok::space::XS)
                        .text_color(resting)
                        .cursor_pointer()
                        .tooltip(crate::ui_primitives::text_tooltip(tooltip))
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.retry_claude_limits(cx);
                        }))
                        .animated_hover(move |style, delta| {
                            style.text_color(lerp_color(resting, hovered, delta));
                        })
                        .child("\u{21bb}"),
                )
                .into_any_element()
            }
            // The whole row presses `sign in`: at the rail's narrowest a
            // seven-character target is a miss.
            OriginAction::SignIn => {
                let project = self
                    .workspaces
                    .get(self.active_idx)
                    .map(|container| container.title.clone());
                match project {
                    Some(project) => {
                        let resting = ui.dim;
                        let hovered = ui.text;
                        row.role(Role::Button)
                            .aria_label("Sign in to Claude Code")
                            .cursor_pointer()
                            .tooltip(crate::ui_primitives::text_tooltip(format!(
                                "Opens a Claude Code pane in {project}; the CLI asks for your \
                                 login there."
                            )))
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.sign_in_to_claude(cx);
                            }))
                            .child(
                                div()
                                    .id("rail-limits-sign-in")
                                    .flex_none()
                                    .text_color(resting)
                                    .animated_hover(move |style, delta| {
                                        style.text_color(lerp_color(resting, hovered, delta));
                                    })
                                    .child("sign in"),
                            )
                            .into_any_element()
                    }
                    // No project, so no pane to open. The control stays where
                    // it is, dimmed, and says why - it does not vanish.
                    None => row
                        .tooltip(crate::ui_primitives::text_tooltip(
                            "Open a project first - signing in happens in a Claude Code pane",
                        ))
                        .child(div().flex_none().child("sign in"))
                        .into_any_element(),
                }
            }
            OriginAction::Retry => row
                .child(retry_button(ui, last_read, cx))
                .into_any_element(),
            OriginAction::None => row.into_any_element(),
        }
    }
}

/// The one-line form of a vendor's row: who, which window, how much, when it
/// comes back. No meter.
///
/// **Presence is what tells the two rows apart** - the full row above has a
/// meter and this one does not - and nothing else does. Same ink, same size,
/// same words; a second vendor is not a lesser one, it is the one you are not
/// working in this minute.
fn render_compact_limits_row(
    row: &VendorRowState,
    held: &[HeldForecast],
    ui: crate::theme::UiColors,
    now: i64,
) -> AnyElement {
    let copy = footer_copy(&row.reading, row.previous.as_ref(), held, row.shown, now);
    // The row's own state word keeps its colour here too: `nearly out` on a
    // vendor you are not looking at is exactly the thing this row exists for.
    let word_color = copy.level.color(ui);
    // A forecast replaces the reset here, as it does on the other window's
    // line of the full row.
    let tail = if copy.detail_aside.is_empty() {
        copy.detail.clone()
    } else {
        copy.detail_aside.clone()
    };
    let value = match (copy.value.is_empty(), tail.is_empty()) {
        (true, _) => tail,
        (false, true) => copy.value.clone(),
        (false, false) => format!("{} \u{b7} {}", copy.value, tail),
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::SM)
        .pt(tok::space::XS)
        .overflow_hidden()
        .font_family(tok::font::MONO)
        .text_size(tok::mono::LABEL)
        .text_color(ui.muted)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(copy.label)),
        )
        .children(copy.level.word().map(|word| {
            div()
                .flex_none()
                .text_color(word_color)
                .child(SharedString::from(word))
        }))
        .children(
            copy.note
                .map(|note| div().flex_none().text_color(ui.dim).child(note)),
        )
        .child(
            div()
                .flex_none()
                // Same rule as the full row: an old number is not stated as the
                // present, whichever row it is in.
                .text_color(if copy.stale { ui.dim } else { ui.muted })
                .child(SharedString::from(value)),
        )
        .into_any_element()
}

fn retry_button(
    ui: crate::theme::UiColors,
    last_read: Option<u64>,
    cx: &mut Context<SplitlaneApp>,
) -> AnyElement {
    let resting = ui.dim;
    let hovered = ui.text;
    let tooltip = match last_read {
        Some(at) => format!("Read the limits again (last attempt {})", clock_hhmm(at)),
        None => "Read the limits now".to_string(),
    };
    div()
        .id("rail-limits-retry")
        .role(Role::Button)
        .aria_label("Retry reading the Claude limits")
        .flex_none()
        .text_color(resting)
        .tooltip(crate::ui_primitives::text_tooltip(tooltip))
        .cursor_pointer()
        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
            this.retry_claude_limits(cx);
        }))
        .animated_hover(move |style, delta| {
            style.text_color(lerp_color(resting, hovered, delta));
        })
        .child("retry now")
        .into_any_element()
}

/// `HH:MM` in the machine's local time, from Unix millis.
///
/// `pub(crate)` for the status bar's restore indicator, which states a time of
/// day for the same reason this footer does and has no business owning a
/// second clock.
pub(crate) fn clock_hhmm(unix_ms: u64) -> String {
    hhmm((unix_ms / 1000) as i64 + local_utc_offset_secs())
}

/// `HH:MM` for a count of seconds since a local midnight-aligned epoch.
/// Split out from [`clock_hhmm`] so the arithmetic - which is the only part
/// that can be wrong - is testable without a clock.
fn hhmm(local_secs: i64) -> String {
    let secs_into_day = local_secs.rem_euclid(86_400);
    format!(
        "{:02}:{:02}",
        secs_into_day / 3600,
        (secs_into_day % 3600) / 60
    )
}

/// Seconds east of UTC where this machine is. `std` has no local-time API and
/// the app already depends on `chrono` for its logging setup.
fn local_utc_offset_secs() -> i64 {
    chrono::Local::now().offset().local_minus_utc() as i64
}

pub(crate) fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_unix_secs() -> i64 {
    (now_unix_millis() / 1000) as i64
}

/// Where the numbers actually come from, for Settings -> Limits.
///
/// The CLI is the only source: the app sends the CLI's own token to the same
/// endpoint the CLI reads, and keeps no counter of its own. What varies is
/// which login that is, so the hint names the config directory when
/// `CLAUDE_CONFIG_DIR` has moved it - a usage counter reading a different
/// login from the one the sessions use is exactly the failure this says out
/// loud rather than leaves to be discovered.
pub(crate) fn usage_source_hint() -> String {
    match crate::claude_usage::credentials::config_dir_override() {
        Some(dir) => format!("its credentials, from {dir}"),
        None => "its own credentials".to_string(),
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    /// The two nothings. Before this, both of them drew a panel that stated
    /// "Claude limits unavailable" - for three seconds at every launch, and
    /// forever for anyone who has never signed into Claude Code.
    #[test]
    fn the_panel_is_absent_when_it_has_nothing_true_to_say() {
        assert!(
            !limits_footer_has_something_to_say(None, true),
            "before the first read the panel knows nothing, and said so as if \
             it were a fact about the account"
        );
        assert!(
            !limits_footer_has_something_to_say(Some(&Err(LimitsError::NotSignedIn)), false),
            "no credentials anywhere means there is no account to report on"
        );
    }

    /// Every other error is a fact about a real account, and the retry button
    /// lives inside the panel - hiding these would take it away too.
    #[test]
    fn a_real_account_with_a_problem_keeps_its_panel() {
        for err in [
            LimitsError::ExpiredLogin,
            LimitsError::RateLimited,
            LimitsError::Network,
            LimitsError::BadReply,
        ] {
            assert!(
                limits_footer_has_something_to_say(Some(&Err(err)), false),
                "{err:?} is something the user can act on"
            );
        }
        assert!(limits_footer_has_something_to_say(
            Some(&Ok(UsageSnapshot {
                five_hour: None,
                seven_day: None,
            })),
            false
        ));
    }

    /// Somebody with the CLI installed and no login is a Claude user
    /// who has not signed in yet, and the panel is where they sign in.
    #[test]
    fn not_signed_in_keeps_its_panel_when_the_cli_is_installed() {
        assert!(limits_footer_has_something_to_say(
            Some(&Err(LimitsError::NotSignedIn)),
            true
        ));
        // And without the CLI it is still nothing: a Codex-only user is not
        // told about a product they do not have.
        assert!(!limits_footer_has_something_to_say(
            Some(&Err(LimitsError::NotSignedIn)),
            false
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_usage::UsageWindow;
    use crate::vendor_limits::{CLAUDE_SESSION_MINUTES, CLAUDE_WEEKLY_MINUTES, from_claude};

    /// The moment every fixture below was read at, in Unix seconds: 08:00.
    const READ_AT: i64 = 8 * 3600;

    /// Claude's session window, as the argument the age rule takes.
    const FIVE_HOURS: Option<i64> = Some(CLAUDE_SESSION_MINUTES);

    fn window(utilization: f32, resets_at: Option<i64>) -> UsageWindow {
        UsageWindow {
            utilization,
            resets_at,
        }
    }

    /// One reply from the endpoint, in the shape the footer reads.
    fn read(five: Option<UsageWindow>, seven: Option<UsageWindow>) -> VendorLimits {
        from_claude(
            &Ok(UsageSnapshot {
                five_hour: five,
                seven_day: seven,
            }),
            READ_AT,
        )
    }

    /// A window of Claude's session length, for the rules that sort by length.
    fn session(utilization: f32, resets_at: Option<i64>) -> Limit {
        Limit {
            utilization,
            window_minutes: Some(CLAUDE_SESSION_MINUTES),
            resets_at,
        }
    }

    /// And one of its weekly length.
    fn weekly(utilization: f32, resets_at: Option<i64>) -> Limit {
        Limit {
            utilization,
            window_minutes: Some(CLAUDE_WEEKLY_MINUTES),
            resets_at,
        }
    }

    /// Which window `choose_limit` picked, by length.
    fn chose(previous: Option<i64>, five: f32, seven: f32) -> Option<i64> {
        choose_limit(
            previous.map(Some),
            &[weekly(seven, None), session(five, None)],
            &[],
        )
        .and_then(|limit| limit.window_minutes)
    }

    /// A verdict needs evidence: "unavailable" is what a read that came back
    /// and failed earns, and before that there is no row at all rather than a
    /// row asserting something about an account nobody has asked about.
    #[test]
    fn nothing_is_unavailable_until_a_read_has_failed() {
        assert!(!limits_footer_has_something_to_say(None, true));
        let failed = from_claude(&Err(LimitsError::ExpiredLogin), READ_AT);
        assert_eq!(
            footer_copy(&failed, None, &[], None, READ_AT).label,
            "Claude limits unavailable"
        );
    }

    #[test]
    fn the_clock_reads_hours_and_minutes() {
        assert_eq!(hhmm(0), "00:00");
        assert_eq!(hhmm(8 * 3600 + 12 * 60), "08:12");
        assert_eq!(hhmm(23 * 3600 + 59 * 60 + 59), "23:59");
    }

    /// A negative local offset west of UTC pushes the count below zero on the
    /// first hours of a UTC day; the day has to wrap rather than format a
    /// negative hour.
    #[test]
    fn the_clock_wraps_instead_of_going_negative() {
        assert_eq!(hhmm(-3600), "23:00");
        assert_eq!(hhmm(-86_400), "00:00");
        assert_eq!(hhmm(86_400 + 3600), "01:00");
    }

    /// A working read is re-taken on focus at most every five minutes, and a
    /// run of failures pushes that out to the poll interval rather than
    /// turning a user who tabs about into a poller.
    #[test]
    fn the_focus_floor_backs_off_and_stops_at_the_poll_interval() {
        assert_eq!(focus_floor(0), FOCUS_FLOOR);
        assert_eq!(focus_floor(1), FOCUS_FLOOR * 2);
        assert_eq!(focus_floor(2), FOCUS_FLOOR * 4);
        assert_eq!(focus_floor(3), LIMITS_POLL_INTERVAL);
        // However long the failures run, it never exceeds one poll interval -
        // past that the scheduled read is doing the work anyway.
        assert_eq!(focus_floor(99), LIMITS_POLL_INTERVAL);
        assert!(focus_floor(0) < LIMITS_POLL_INTERVAL);
    }

    #[test]
    fn spans_use_the_two_coarsest_units() {
        assert_eq!(format_span(30), "<1m");
        assert_eq!(format_span(38 * 60), "38m");
        assert_eq!(format_span(3 * 3600 + 32 * 60), "3h 32m");
        assert_eq!(format_span(2 * 86_400 + 4 * 3600), "2d 4h");
        assert_eq!(format_span(7 * 86_400), "7d 0h");
    }

    // --- the auto-switch ---

    /// With nothing shown before, the prototype's plain rule decides.
    #[test]
    fn the_first_reading_uses_the_plain_rule() {
        assert_eq!(chose(None, 10.0, 60.0), Some(CLAUDE_WEEKLY_MINUTES));
        assert_eq!(chose(None, 66.0, 60.0), Some(CLAUDE_SESSION_MINUTES));
        assert_eq!(chose(None, 63.0, 60.0), Some(CLAUDE_WEEKLY_MINUTES));
        assert_eq!(chose(None, 80.0, 95.0), Some(CLAUDE_SESSION_MINUTES));
    }

    /// The rule reads the windows by length, not by the order they arrive in.
    /// Codex states `primary` and `secondary` and says nothing about which is
    /// shorter; if the order mattered, its row would pick the wrong window
    /// exactly when the two disagree.
    #[test]
    fn the_shorter_window_is_the_privileged_one_whatever_order_it_arrives_in() {
        let forwards = choose_limit(None, &[session(66.0, None), weekly(60.0, None)], &[]);
        let backwards = choose_limit(None, &[weekly(60.0, None), session(66.0, None)], &[]);
        assert_eq!(forwards, backwards);
        assert_eq!(
            forwards.and_then(|limit| limit.window_minutes),
            Some(CLAUDE_SESSION_MINUTES)
        );
    }

    /// One window is the whole answer, and no window is no answer - which is
    /// the state a source we overhear reaches by having nothing current left.
    #[test]
    fn a_single_window_is_shown_and_none_is_nothing() {
        assert_eq!(
            choose_limit(None, &[weekly(4.0, None)], &[]).and_then(|l| l.window_minutes),
            Some(CLAUDE_WEEKLY_MINUTES)
        );
        assert_eq!(choose_limit(Some(Some(300)), &[], &[]), None);
    }

    #[test]
    fn a_forecast_is_only_announced_while_it_is_early() {
        // The three states that are not `Early` must produce nothing, and the
        // one that is must produce something. This is the whole trigger, kept
        // as a statement about `forecast` rather than about the dispatcher, so
        // it cannot be read as testing the plumbing.
        let now = 10_000;
        let climbing = Some((0.0f32, 9_000i64));
        assert!(matches!(
            forecast(50.0, now, Some(12_000), climbing, Some(now), FIVE_HOURS),
            Forecast::Early { .. }
        ));
        assert!(!matches!(
            forecast(50.0, now, Some(10_400), climbing, Some(now), FIVE_HOURS),
            Forecast::Early { .. }
        ));
        assert!(!matches!(
            forecast(50.0, now, Some(12_000), None, Some(now), FIVE_HOURS),
            Forecast::Early { .. }
        ));
        assert!(!matches!(
            forecast(
                50.0,
                now,
                Some(12_000),
                climbing,
                Some(now - stale_after(FIVE_HOURS) - 1),
                FIVE_HOURS,
            ),
            Forecast::Early { .. }
        ));
    }

    #[test]
    fn the_left_slot_is_the_forecast_and_the_right_is_the_reset() {
        // One meaning per slot. The left is the one text in the
        // footer about the future; the right is when the window comes back.
        let now = 10_000i64;
        let reading = |utilization: f32, resets: i64| VendorLimits {
            observed_at: Some(now),
            ..from_claude(
                &Ok(UsageSnapshot {
                    five_hour: Some(window(utilization, Some(resets))),
                    seven_day: None,
                }),
                now,
            )
        };

        // No rate: nothing to project, so the left slot is empty.
        let copy = footer_copy(&reading(50.0, 12_000), None, &[], None, now);
        assert_eq!(copy.detail_aside, "");
        assert!(copy.detail.starts_with("resets in"), "{}", copy.detail);

        // Early: the forecast on the left, and the reset as a clock on the
        // right, because the offset is measured back from that moment.
        let climbing = PreviousReading {
            at: 9_000,
            windows: vec![(Some(CLAUDE_SESSION_MINUTES), 0.0)],
        };
        let copy = footer_copy(&reading(50.0, 12_000), Some(&climbing), &[], None, now);
        assert_eq!(copy.detail_aside, "out 16m early");
        assert_eq!(copy.detail, format!("resets {}", clock_hhmm(12_000 * 1000)));

        // Benign: nothing to say on the left.
        let copy = footer_copy(&reading(50.0, 10_400), Some(&climbing), &[], None, now);
        assert_eq!(copy.detail_aside, "");
        assert!(copy.detail.starts_with("resets in"), "{}", copy.detail);
    }

    #[test]
    fn a_stale_reading_greys_and_leaves_its_age_to_the_origin_line() {
        // The left slot is the forecast and nothing else, so staleness is not
        // said there any more: the number greys, and the origin line - which
        // states the moment of the read in every state - carries the clock.
        let now = 10_000i64;
        let stale_at = now - stale_after(FIVE_HOURS) - 60;
        let reading = VendorLimits {
            observed_at: Some(stale_at),
            ..from_claude(
                &Ok(UsageSnapshot {
                    five_hour: Some(window(50.0, Some(now + 4_000))),
                    seven_day: None,
                }),
                stale_at,
            )
        };
        let previous = PreviousReading {
            at: stale_at - 1_000,
            windows: vec![(Some(CLAUDE_SESSION_MINUTES), 10.0)],
        };
        let copy = footer_copy(&reading, Some(&previous), &[], None, now);
        assert_eq!(copy.detail_aside, "");
        assert!(copy.stale, "an old reading is not drawn as a current one");
        let origin = origin_line(&reading, false);
        assert_eq!(
            origin.text,
            format!("read {}", clock_hhmm(stale_at as u64 * 1000))
        );
        assert_eq!(origin.action, OriginAction::Refresh);
    }

    #[test]
    fn one_reading_is_not_a_rate() {
        // Half an hour of waiting is the honest state, not a benign one: the
        // footer says `no rate yet` rather than drawing a comfortable forecast
        // out of a single number.
        assert_eq!(
            forecast(63.0, 10_000, Some(20_000), None, Some(10_000), FIVE_HOURS),
            Forecast::NoRate
        );
    }

    #[test]
    fn a_window_that_went_down_is_a_new_window_not_a_negative_rate() {
        // The reset happened between the two readings. The old window's slope
        // says nothing about the new one, and a negative rate would project a
        // reset that already happened.
        assert_eq!(
            forecast(
                4.0,
                10_000,
                Some(20_000),
                Some((90.0, 8_000)),
                Some(10_000),
                FIVE_HOURS
            ),
            Forecast::NoRate
        );
    }

    #[test]
    fn a_window_spent_before_its_reset_says_how_much_earlier() {
        // 50% in 1000s is 0.05%/s; the remaining 50 points take another 1000s,
        // so it is full at 11 000 and resets at 12 000 - a thousand seconds
        // early.
        let f = forecast(
            50.0,
            10_000,
            Some(12_000),
            Some((0.0, 9_000)),
            Some(10_000),
            FIVE_HOURS,
        );
        assert_eq!(f, Forecast::Early { seconds: 1_000 });
    }

    #[test]
    fn a_window_that_outlasts_its_reset_makes_no_claim_about_it() {
        // The same climb against a reset that is closer than the projection.
        assert_eq!(
            forecast(
                50.0,
                10_000,
                Some(10_500),
                Some((0.0, 9_000)),
                Some(10_000),
                FIVE_HOURS
            ),
            Forecast::Survives
        );
    }

    #[test]
    fn a_reading_two_poll_intervals_old_is_not_projected_from() {
        // One missed read is a laptop that slept; two in a row is a source that
        // stopped answering, and a projection from it would keep asserting a
        // trend nobody has checked.
        let now = 10_000;
        let stale = now - stale_after(FIVE_HOURS) - 1;
        assert_eq!(
            forecast(
                63.0,
                now,
                Some(20_000),
                Some((10.0, stale - 1_000)),
                Some(stale),
                FIVE_HOURS,
            ),
            Forecast::Stale
        );
        // **The same age against a longer window is not stale at all.** An
        // hour is most of a five-hour window and half a per cent of a week, so
        // one threshold in seconds answered one of them wrongly whichever
        // number it held.
        assert_ne!(
            forecast(
                63.0,
                now,
                Some(2_000_000),
                Some((10.0, stale - 1_000)),
                Some(stale),
                Some(CLAUDE_WEEKLY_MINUTES),
            ),
            Forecast::Stale
        );
        // And a source that never stated its window length keeps the rule the
        // fraction replaced: two poll intervals.
        assert_eq!(
            forecast(
                63.0,
                now,
                Some(2_000_000),
                Some((10.0, now - STALE_WITHOUT_A_LENGTH - 2_000)),
                Some(now - STALE_WITHOUT_A_LENGTH - 1),
                None,
            ),
            Forecast::Stale
        );
    }

    /// **The rate belongs to the two readings, not to when somebody looked at
    /// the row.**
    ///
    /// Found by cross-vendor review, 9 September, and older than the change it
    /// was found in. The slope divided the climb between two readings by the
    /// time since the *older* one **up to now**, so the same pair of numbers
    /// projected differently every second the row stayed on screen: at the
    /// instant of the read the divisor was the real interval, and by the next
    /// poll it had grown to twice it, halving the reported rate. The companion
    /// half of the same defect is on the other side of the division and not
    /// visible from here - `previous.at` was stamped with the moment of the
    /// read replacing it, making the divisor zero at the instant of the read,
    /// which is why the projection could never announce anything at all.
    #[test]
    fn the_slope_is_measured_between_the_readings_not_up_to_now() {
        // Ten points in half an hour: 50 points left take 9 000 s, and the
        // window resets 12 000 s after the reading, so it is spent 3 000 s
        // early.
        let observed = 100_000;
        let climbing = Some((40.0f32, observed - 1_800));
        assert_eq!(
            forecast(
                50.0,
                observed,
                Some(observed + 12_000),
                climbing,
                Some(observed),
                FIVE_HOURS,
            ),
            Forecast::Early { seconds: 3_000 }
        );
        // Ten minutes later, nothing has been read and nothing has changed
        // about the rate: the wall is at the same moment, so it arrives 600
        // seconds less early. Measured against `now`, the divisor would have
        // grown to 2 400 s and the same reading would have projected the
        // window as surviving.
        assert_eq!(
            forecast(
                50.0,
                observed + 600,
                Some(observed + 12_000),
                climbing,
                Some(observed),
                FIVE_HOURS,
            ),
            Forecast::Early { seconds: 2_400 }
        );
    }

    /// A reading whose moment is unknown is not a current one.
    #[test]
    fn a_reading_with_no_moment_is_stale_rather_than_fresh() {
        assert_eq!(
            forecast(
                50.0,
                10_000,
                Some(20_000),
                Some((10.0, 9_000)),
                None,
                FIVE_HOURS
            ),
            Forecast::Stale
        );
    }

    /// The threshold, as the two numbers it is calibrated against.
    #[test]
    fn a_reading_goes_stale_in_proportion_to_the_window_it_is_about() {
        // A fifth of five hours is the hour this rule replaced, exactly.
        assert_eq!(
            stale_after(FIVE_HOURS),
            2 * LIMITS_POLL_INTERVAL.as_secs() as i64
        );
        // A fifth of a week is a day and a half, which is the point: a weekly
        // figure does not stop being true because one poll was missed.
        assert_eq!(stale_after(Some(CLAUDE_WEEKLY_MINUTES)), 7 * 24 * 3600 / 5);
        // And never sooner than one poll, so a source we ask cannot be stale
        // between two successful reads of it.
        assert_eq!(stale_after(Some(5)), LIMITS_POLL_INTERVAL.as_secs() as i64);
        assert_eq!(stale_after(None), STALE_WITHOUT_A_LENGTH);
    }

    #[test]
    fn a_window_is_named_by_its_length_and_the_word_is_only_used_where_it_is_exact() {
        // The table as the design states it. A word only where the word means
        // exactly that length; everything else its own number in the largest
        // unit that divides it.
        for (minutes, expected) in [
            (60, "hourly"),
            (300, "5-hour"),
            (1440, "daily"),
            (10080, "weekly"),
            (90, "90-minute"),
            (4320, "3-day"),
            (43200, "30-day"),
        ] {
            assert_eq!(
                window_name(Some(minutes)).as_deref(),
                Some(expected),
                "{minutes} minutes"
            );
        }
    }

    #[test]
    fn thirty_days_is_not_a_month_and_seven_days_is_not_seven_days() {
        // Two arms of the rule that exist to prevent a specific wrong word.
        // `monthly` would promise a reset on the first of the month; `7-day`
        // would be the general arm swallowing a length that has an exact name.
        assert_eq!(window_name(Some(43200)).as_deref(), Some("30-day"));
        assert_eq!(window_name(Some(10080)).as_deref(), Some("weekly"));
        // And the two that would come out as "1-hour"/"1-day" without their
        // own arms.
        assert_eq!(window_name(Some(60)).as_deref(), Some("hourly"));
        assert_eq!(window_name(Some(1440)).as_deref(), Some("daily"));
    }

    #[test]
    fn a_window_without_a_length_has_no_name_rather_than_an_invented_one() {
        assert_eq!(window_name(None), None);
        // Not a length. Zero divides by everything, so without this guard the
        // general arm would confidently answer "0-day".
        assert_eq!(window_name(Some(0)), None);
        assert_eq!(window_name(Some(-300)), None);
    }

    #[test]
    fn the_rule_returns_the_two_strings_that_used_to_be_hardcoded() {
        // Why this vendor's rows are labelled by a rule written for the other
        // one: applied to these two windows it reproduces the copy exactly, so
        // nothing on screen moved and two string constants left the source.
        assert_eq!(
            limit_label(Some("Claude"), Some(CLAUDE_SESSION_MINUTES)),
            "Claude 5-hour limit"
        );
        assert_eq!(
            limit_label(None, Some(CLAUDE_WEEKLY_MINUTES)),
            "weekly limit"
        );
    }

    #[test]
    fn a_row_whose_window_has_no_name_says_less_rather_than_something_else() {
        // Unreachable for this vendor, whose two lengths are known by
        // construction; it is the shape the second vendor's row takes when the
        // server states a percentage and no window length.
        assert_eq!(limit_label(Some("Codex"), None), "Codex limit");
        assert_eq!(limit_label(None, None), "other limit");
        assert_eq!(
            limit_label(Some("Codex"), Some(90)),
            "Codex 90-minute limit"
        );
    }

    #[test]
    fn the_label_does_not_flap_when_the_values_cross() {
        let mut shown = chose(None, 66.0, 60.0);
        assert_eq!(shown, Some(CLAUDE_SESSION_MINUTES));
        // The week creeps up past the session but not clear of it.
        for week in [61.0, 63.0, 65.5, 64.0, 62.0] {
            shown = chose(shown, 66.0, week);
            assert_eq!(shown, Some(CLAUDE_SESSION_MINUTES), "weekly {week}");
        }
        // Only once the week is level does the footer hand over.
        assert_eq!(chose(shown, 66.0, 66.0), Some(CLAUDE_WEEKLY_MINUTES));
    }

    /// And back the other way: from the week, the session needs the full
    /// five-point lead, not merely to be ahead.
    #[test]
    fn coming_back_the_other_way_needs_the_full_lead() {
        let shown = Some(CLAUDE_WEEKLY_MINUTES);
        assert_eq!(chose(shown, 62.0, 60.0), Some(CLAUDE_WEEKLY_MINUTES));
        assert_eq!(chose(shown, 65.0, 60.0), Some(CLAUDE_WEEKLY_MINUTES));
        assert_eq!(chose(shown, 65.1, 60.0), Some(CLAUDE_SESSION_MINUTES));
    }

    /// A session near its cap binds whatever the week is doing, and holds.
    #[test]
    fn a_session_near_the_cap_latches() {
        assert_eq!(
            chose(Some(CLAUDE_WEEKLY_MINUTES), 80.0, 99.0),
            Some(CLAUDE_SESSION_MINUTES)
        );
        assert_eq!(
            chose(Some(CLAUDE_SESSION_MINUTES), 80.0, 99.0),
            Some(CLAUDE_SESSION_MINUTES)
        );
    }

    /// A limit that is reached is the one to read about, memory or not - and
    /// of two reached windows the shorter one, which is the wall you meet
    /// first.
    #[test]
    fn a_reached_limit_wins_outright() {
        assert_eq!(
            chose(Some(CLAUDE_WEEKLY_MINUTES), 100.0, 99.0),
            Some(CLAUDE_SESSION_MINUTES)
        );
        assert_eq!(
            chose(Some(CLAUDE_SESSION_MINUTES), 10.0, 100.0),
            Some(CLAUDE_WEEKLY_MINUTES)
        );
        assert_eq!(
            chose(Some(CLAUDE_WEEKLY_MINUTES), 100.0, 100.0),
            Some(CLAUDE_SESSION_MINUTES)
        );
    }

    // --- the copy ---

    #[test]
    fn a_read_footer_names_the_limit_the_reset_and_the_other_one() {
        let limits = read(
            Some(window(63.0, Some(1_000 + 38 * 60))),
            Some(window(21.0, Some(1_000 + 2 * 86_400 + 4 * 3600))),
        );
        let copy = footer_copy(&limits, None, &[], Some(FIVE_HOURS), 1_000);
        assert_eq!(copy.label, "Claude 5-hour limit");
        assert_eq!(copy.value, "63%");
        assert_eq!(copy.detail, "resets in 38m");
        assert_eq!(copy.other_label, "weekly limit");
        assert_eq!(copy.other_value, "21% · 2d 4h");
        assert_eq!(copy.level, Level::Quiet);
        // The quiet band says nothing, and the name carries no state.
        assert_eq!(copy.level.word(), None);
    }

    /// The design's own "limit reached" state. The word was moved out of
    /// the label, so the name is now plain and the state stands on its own.
    #[test]
    fn a_reached_limit_says_so_and_fills_the_meter() {
        let limits = read(
            Some(window(100.0, Some(1_000 + 38 * 60))),
            Some(window(21.0, None)),
        );
        let copy = footer_copy(&limits, None, &[], Some(FIVE_HOURS), 1_000);
        assert_eq!(copy.label, "Claude 5-hour limit");
        assert_eq!(copy.level.word(), Some("reached"));
        assert_eq!(copy.value, "100%");
        assert_eq!(copy.fill, Some(1.0));
        assert_eq!(copy.level, Level::Reached);
        assert_eq!(copy.detail, "resets in 38m");
    }

    /// The number on screen and the band it is coloured by are the same
    /// reading, at both thresholds.
    ///
    /// Found by cross-vendor review of the threshold change: the value was
    /// rounded for display and the band was taken from the raw float, so a
    /// window at 99.6 rendered `100%` beside `nearly out`, and one at 84.7
    /// rendered `85%` in silence while Settings -> Limits stated the warning
    /// begins at 85%. Both are the row contradicting itself in the one place
    /// a reader can check it against.
    #[test]
    fn the_number_on_screen_and_the_band_are_the_same_reading() {
        let band = |utilization: f32| {
            let limits = read(Some(window(utilization, None)), Some(window(1.0, None)));
            let copy = footer_copy(&limits, None, &[], Some(FIVE_HOURS), 1_000);
            (copy.value, copy.level)
        };

        // Rounds up into the warning band: the row says 85%, so the row warns.
        assert_eq!(band(84.7), ("85%".to_string(), Level::NearlyOut));
        // Rounds up to the cap: the row says 100%, so the row says `reached`.
        assert_eq!(band(99.6), ("100%".to_string(), Level::Reached));
        // Rounds down and stays quiet, for the same reason.
        assert_eq!(band(84.4), ("84%".to_string(), Level::Quiet));
        assert_eq!(band(99.4), ("99%".to_string(), Level::NearlyOut));
    }

    #[test]
    fn the_meter_changes_band_at_eighty_five_and_a_hundred() {
        assert_eq!(Level::of(84.9), Level::Quiet);
        assert_eq!(Level::of(85.0), Level::NearlyOut);
        assert_eq!(Level::of(99.9), Level::NearlyOut);
        assert_eq!(Level::of(100.0), Level::Reached);
        // The deleted band: 70% used to be a colour with no word.
        assert_eq!(Level::of(70.0), Level::Quiet);
    }

    /// The rule, as a test rather than as prose: colour and word begin
    /// at the same two thresholds, and the band below them has neither.
    ///
    /// This is the machine form of "no colour-only signal". A future change
    /// that colours a band without giving it a word fails here, which is the
    /// only place that can notice - nothing else in the suite can see a
    /// painted pixel.
    ///
    /// It runs over **every bundled theme**, not the ambient one. Asking
    /// `ui_colors()` would have made the answer depend on which theme happens
    /// to be active on the machine running the suite - a user theme on disk,
    /// hot-reloaded mid-run - so the test could pass here and mean nothing, or
    /// fail on somebody's laptop for a reason that is not this code.
    #[test]
    fn every_coloured_band_has_a_word_and_the_quiet_one_has_neither() {
        for (name, build) in crate::theme::THEMES {
            let ui = crate::theme::ui_colors_with(&build());

            // The quiet band: no word, and the neutral role rather than a
            // colour of its own.
            assert_eq!(Level::Quiet.word(), None, "{name}");
            assert_eq!(Level::Quiet.color(ui), ui.dim, "{name}");

            for level in [Level::NearlyOut, Level::Reached] {
                assert!(
                    level.word().is_some(),
                    "{name}: {level:?} is coloured, so it must carry a word"
                );
                assert_ne!(
                    level.color(ui),
                    ui.dim,
                    "{name}: {level:?} must be distinguishable from the quiet band"
                );
            }

            // No band may take the accent. That hue answers one question in
            // this app - does something want me? - and it keeps answering it
            // only while nothing else spends it. The quiet band spending it
            // was the old scheme's whole defect; an error band spending it would be
            // the same mistake at the other end.
            for level in [Level::Quiet, Level::NearlyOut, Level::Reached] {
                assert_ne!(level.color(ui), ui.accent, "{name}: {level:?}");
            }
        }
    }

    /// Every failure of a read that could have succeeded lands in the design's
    /// degraded state, and the origin line names its cause beside the one
    /// action that can answer it.
    #[test]
    fn every_failure_is_the_degraded_state_with_a_reason() {
        for (err, action) in [
            (LimitsError::ExpiredLogin, OriginAction::SignIn),
            (LimitsError::RateLimited, OriginAction::Retry),
            (LimitsError::Network, OriginAction::Retry),
            (LimitsError::BadReply, OriginAction::Retry),
        ] {
            let reading = from_claude(&Err(err), READ_AT);
            let copy = footer_copy(&reading, None, &[], None, 0);
            assert_eq!(copy.label, "Claude limits unavailable");
            assert_eq!(copy.value, "—");
            assert_eq!(copy.fill, None);
            // The reading was there and went away, so the bare track stays.
            assert!(copy.meter);
            // **And nothing about a window it never read.**
            assert_eq!(copy.other_label, "");
            assert_eq!(copy.other_value, "");
            let origin = origin_line(&reading, false);
            assert_eq!(origin.text, err.reason());
            // `retry now` only where a second attempt can work: reading dead
            // credentials again cannot, so an expired login signs in.
            assert_eq!(origin.action, action, "{err:?}");
        }
    }

    /// Never read is not "read and lost": one line, no meter, and the way in.
    #[test]
    fn not_signed_in_is_one_line_with_the_way_in() {
        let reading = from_claude(&Err(LimitsError::NotSignedIn), READ_AT);
        let copy = footer_copy(&reading, None, &[], None, READ_AT);
        assert_eq!(copy.label, "Claude limit");
        assert_eq!(copy.note, Some("not signed in"));
        assert!(!copy.meter, "a meter with no quantity promises one");
        assert_eq!(copy.value, "");
        let origin = origin_line(&reading, false);
        assert_eq!(origin.text, "nothing read yet");
        assert_eq!(origin.action, OriginAction::SignIn);
    }

    /// The origin line stands in every state, and in flight it says so while
    /// the numbers above it stay.
    #[test]
    fn the_origin_line_always_says_something() {
        let read = read(Some(window(40.0, Some(READ_AT + 3_600))), None);
        let origin = origin_line(&read, false);
        assert_eq!(
            origin.text,
            format!("read {}", clock_hhmm(READ_AT as u64 * 1000))
        );
        assert_eq!(origin.action, OriginAction::Refresh);
        let reading = origin_line(&read, true);
        assert_eq!(reading.text, "reading\u{2026}");
        // The `↻` stays in flight so nothing moves under the pointer.
        assert_eq!(reading.action, OriginAction::Refresh);
    }

    /// A source that never said when it was speaking gets no clock invented
    /// for it: the line carries the cause alone.
    #[test]
    fn a_reading_with_no_moment_behind_it_names_none() {
        let undated = VendorLimits {
            observed_at: None,
            ..from_claude(&Err(LimitsError::Network), READ_AT)
        };
        assert_eq!(origin_line(&undated, false).text, "no network");
    }

    /// A reset time already in the past is not printed as a negative span; the
    /// footer falls back to when it read.
    #[test]
    fn a_stale_reset_time_is_not_printed() {
        let limits = read(Some(window(10.0, Some(500))), None);
        let copy = footer_copy(&limits, None, &[], Some(FIVE_HOURS), 1_000);
        // The window it named has already started over, so there is no window
        // to draw at all: the row says when the vendor last spoke and states
        // no percentage. It used to print the reading anyway, which is right
        // for a source polled every half hour and wrong for one that may be
        // repeating a record from last week - see `Limit::is_current`.
        assert_eq!(copy.label, "Claude limits");
        assert_eq!(copy.detail, "no current window");
        assert_eq!(copy.value, "\u{2014}");
        assert_eq!(copy.fill, None);
    }

    /// The second vendor's own states, through the same copy the first one
    /// uses: a login billed by a key, and a source we overhear that has
    /// nothing current to say.
    #[test]
    fn a_login_with_no_plan_window_says_so_and_draws_no_meter() {
        let codex = crate::vendor_limits::from_codex(
            &crate::codex_sessions::CodexAccountLimits::NoPlanWindow { observed_at: 900 },
        );
        let copy = footer_copy(&codex, None, &[], None, 1_000);
        assert_eq!(copy.label, "Codex \u{2014} no plan limit");
        assert_eq!(
            copy.detail,
            "billed by the key \u{2014} no plan limit to read"
        );
        // The label never asserts a limit the line under it denies, and there
        // is no quantity, so there is no meter.
        assert_eq!(copy.fill, None);
        assert_eq!(copy.value, "");
        assert_eq!(copy.level, Level::Quiet);
    }

    /// A source we overhear names **its own** moment, not ours.
    #[test]
    fn the_second_vendors_row_dates_itself_by_when_the_agent_last_spoke() {
        let codex =
            crate::vendor_limits::from_codex(&crate::codex_sessions::CodexAccountLimits::Windows {
                limits: crate::codex_sessions::CodexRateLimits {
                    limit_id: None,
                    limit_name: None,
                    primary: Some(crate::codex_sessions::CodexRateLimitWindow {
                        used_percent: 88.0,
                        window_minutes: Some(300),
                        resets_at: Some(20_000),
                    }),
                    secondary: None,
                    plan_type: None,
                    rate_limit_reached_type: None,
                },
                observed_at: 1_000,
            });
        // Read four hours later: most of a five-hour window has gone by unseen.
        let now = 1_000 + 4 * 3600;
        let copy = footer_copy(&codex, None, &[], None, now);
        assert_eq!(copy.label, "Codex 5-hour limit");
        assert_eq!(copy.value, "88%");
        // The word and its colour stand - inside one window utilisation only
        // rises, so an old `nearly out` is still at least that - while the
        // number itself stops being stated as the present.
        assert_eq!(copy.level, Level::NearlyOut);
        assert!(copy.stale);
        let origin = origin_line(&codex, false);
        assert_eq!(
            origin.text,
            format!("last activity {}", clock_hhmm(1_000 * 1000)),
            "the vendor's own moment, in the vendor's own word"
        );
        // Nothing to press: the numbers arrive when the agent next runs.
        assert_eq!(origin.action, OriginAction::None);
    }

    /// A slope over a few minutes is noise, and was the reason a
    /// forecast could fire and then contradict itself on the next read.
    #[test]
    fn a_rate_needs_a_quarter_of_an_hour_between_its_readings() {
        let now = 10_000;
        // Ten points in ten minutes would project the window spent early; it
        // is too short an interval to call that a rate.
        assert_eq!(
            forecast(
                60.0,
                now,
                Some(now + 12_000),
                Some((50.0, now - 600)),
                Some(now),
                FIVE_HOURS
            ),
            Forecast::NoRate
        );
        // The same climb over fifteen minutes is one.
        assert!(matches!(
            forecast(
                60.0,
                now,
                Some(now + 12_000),
                Some((50.0, now - MIN_RATE_SPAN)),
                Some(now),
                FIVE_HOURS
            ),
            Forecast::Early { .. }
        ));
    }

    /// A read five minutes after the last one does not become the other end of
    /// the slope; the older anchor stays and the interval only grows.
    #[test]
    fn a_quick_second_read_keeps_the_older_anchor() {
        let anchor = PreviousReading {
            at: 1_000,
            windows: vec![],
        };
        assert!(!replaces_anchor(Some(&anchor), 5_000, 5_300));
        assert!(replaces_anchor(Some(&anchor), 5_000, 5_000 + MIN_RATE_SPAN));
        // With nothing to subtract from, anything is better than nothing.
        assert!(replaces_anchor(None, 5_000, 5_010));
    }

    /// A forecast stays on screen until two reads in a row disagree with it,
    /// and goes at once when its window resets.
    #[test]
    fn a_held_forecast_falls_after_two_disagreeing_reads() {
        let now = 10_000;
        let reading = |resets_at: i64| VendorLimits {
            observed_at: Some(now),
            ..from_claude(
                &Ok(UsageSnapshot {
                    five_hour: Some(window(50.0, Some(resets_at))),
                    seven_day: None,
                }),
                now,
            )
        };
        let held = vec![HeldForecast {
            window: FIVE_HOURS,
            resets_at: 12_000,
            seconds: 1_000,
            misses: 0,
        }];
        // A read with no rate does not repeat it: one miss, still held, and
        // still drawn.
        let once = carry_held(&held, &reading(12_000), None, now);
        assert_eq!(once.len(), 1);
        assert_eq!(once[0].misses, 1);
        let copy = footer_copy(&reading(12_000), None, &once, None, now);
        assert_eq!(copy.detail_aside, "out 16m early");
        // A second one takes it down.
        assert!(carry_held(&once, &reading(12_000), None, now).is_empty());
        // A window that turned over takes it down at once.
        assert!(carry_held(&held, &reading(30_000), None, now).is_empty());
        // And a read that repeats it resets the count.
        let climbing = PreviousReading {
            at: 9_000,
            windows: vec![(FIVE_HOURS, 0.0)],
        };
        let again = carry_held(&once, &reading(12_000), Some(&climbing), now);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].misses, 0);
    }

    /// The window that will run out early binds, whichever it is - so the
    /// forecast is always about the window on the meter.
    #[test]
    fn a_window_projected_to_run_out_early_takes_the_meter() {
        let limits = [weekly(78.0, None), session(88.0, None)];
        // By the plain rule the session, at 88, would hold the row.
        assert_eq!(
            choose_limit(None, &limits, &[]).and_then(|l| l.window_minutes),
            Some(CLAUDE_SESSION_MINUTES)
        );
        assert_eq!(
            choose_limit(None, &limits, &[Some(CLAUDE_WEEKLY_MINUTES)])
                .and_then(|l| l.window_minutes),
            Some(CLAUDE_WEEKLY_MINUTES)
        );
        // A reached window still wins over any projection.
        let reached = [weekly(78.0, None), session(100.0, None)];
        assert_eq!(
            choose_limit(None, &reached, &[Some(CLAUDE_WEEKLY_MINUTES)])
                .and_then(|l| l.window_minutes),
            Some(CLAUDE_SESSION_MINUTES)
        );
    }

    /// When both windows run out early, the line of the other one carries its
    /// own forecast in place of its reset.
    #[test]
    fn the_other_window_carries_its_own_forecast() {
        let now = 10_000;
        let limits = VendorLimits {
            observed_at: Some(now),
            ..read(
                Some(window(50.0, Some(12_000))),
                Some(window(50.0, Some(now + 3 * 86_400))),
            )
        };
        let held = [
            HeldForecast {
                window: FIVE_HOURS,
                resets_at: 12_000,
                seconds: 1_000,
                misses: 0,
            },
            HeldForecast {
                window: Some(CLAUDE_WEEKLY_MINUTES),
                resets_at: now + 3 * 86_400,
                seconds: 86_400,
                misses: 0,
            },
        ];
        let copy = footer_copy(&limits, None, &held, None, now);
        assert_eq!(copy.label, "Claude 5-hour limit");
        assert_eq!(copy.other_value, "50% \u{b7} out 1d 0h early");
    }

    /// A moment more than a day off carries its weekday; one inside the day is
    /// a bare clock.
    #[test]
    fn a_far_reset_names_its_day() {
        let now = 1_787_000_000;
        assert_eq!(
            clock_when(now + 3_600, now),
            clock_hhmm((now + 3_600) as u64 * 1000)
        );
        let far = clock_when(now + 3 * 86_400, now);
        assert_eq!(far.len(), "Mon 09:00".len(), "{far}");
        assert!(far.ends_with(&clock_hhmm((now + 3 * 86_400) as u64 * 1000)));
    }

    fn history_at(at: i64, five: f32) -> PreviousReading {
        PreviousReading {
            at,
            windows: vec![(FIVE_HOURS, five), (Some(CLAUDE_WEEKLY_MINUTES), 20.0)],
        }
    }

    fn five_hour_reading(at: i64, five: f32, resets_at: i64) -> VendorLimits {
        VendorLimits {
            observed_at: Some(at),
            ..from_claude(
                &Ok(UsageSnapshot {
                    five_hour: Some(window(five, Some(resets_at))),
                    seven_day: Some(window(21.0, Some(at + 5 * 86_400))),
                }),
                at,
            )
        }
    }

    /// The case this was reported from. A session eats thirty points of the
    /// five-hour window in twenty minutes; at that pace the rest goes within
    /// the hour, long before the window comes back.
    #[test]
    fn a_window_eaten_in_minutes_lights_the_fuse() {
        let now = 100_000;
        let history = [history_at(now - 20 * 60, 10.0)];
        let burn = burn_check(&five_hour_reading(now, 40.0, now + 4 * 3600), &history)
            .expect("thirty points in twenty minutes is a burn");
        assert_eq!(burn.window, FIVE_HOURS);
        assert!((burn.points - 30.0).abs() < f32::EPSILON);
        assert_eq!(burn.span, 20 * 60);
        // Sixty points left at 1.5 points a minute: forty minutes.
        assert_eq!(burn.runs_out_at, now + 40 * 60);
    }

    /// The ordinary day stays silent: a steady pace that lasts past the hour
    /// and a half, small movements that are rounding, and anything older than
    /// the half hour the fuse looks back over.
    #[test]
    fn ordinary_work_does_not_light_the_fuse() {
        let now = 100_000;
        let resets = now + 4 * 3600;
        // Ten points in half an hour from 9%: ninety points take four and a
        // half hours - busy, not burning.
        assert!(
            burn_check(
                &five_hour_reading(now, 19.0, resets),
                &[history_at(now - 30 * 60, 9.0)]
            )
            .is_none()
        );
        // The sighting that started this: 9% of the five-hour window.
        assert!(
            burn_check(
                &five_hour_reading(now, 9.0, resets),
                &[history_at(now - 30 * 60, 4.0)]
            )
            .is_none()
        );
        // A reference older than the look-back measures nothing.
        assert!(
            burn_check(
                &five_hour_reading(now, 60.0, resets),
                &[history_at(now - 2 * 3600, 10.0)]
            )
            .is_none()
        );
        // And one too recent is not a span.
        assert!(
            burn_check(
                &five_hour_reading(now, 60.0, resets),
                &[history_at(now - 60, 10.0)]
            )
            .is_none()
        );
    }

    /// A burst right after a reset is measured from the new window's first
    /// reading, not from the old window's last one.
    #[test]
    fn a_burst_in_a_fresh_window_is_not_hidden_by_the_one_before() {
        let now = 100_000;
        let history = [
            history_at(now - 25 * 60, 85.0),
            // The window reset here.
            history_at(now - 20 * 60, 2.0),
        ];
        let burn = burn_check(&five_hour_reading(now, 40.0, now + 4 * 3600), &history)
            .expect("38 points in twenty minutes of a fresh window");
        assert!((burn.points - 38.0).abs() < f32::EPSILON);
    }

    /// The same five points quicken the poll over ten minutes and not over a
    /// half-hour idle poll.
    #[test]
    fn a_climb_is_judged_by_its_pace() {
        assert!(!climbing_fast(&[
            history_at(0, 10.0),
            history_at(1_800, 15.0)
        ]));
        assert!(climbing_fast(&[history_at(0, 10.0), history_at(600, 15.0)]));
    }

    /// A burn that the reset outruns is no emergency: the window comes back
    /// before the wall.
    #[test]
    fn a_burn_the_reset_outruns_says_nothing() {
        let now = 100_000;
        let history = [history_at(now - 20 * 60, 10.0)];
        assert!(burn_check(&five_hour_reading(now, 40.0, now + 20 * 60), &history).is_none());
    }

    /// The week is never fused, and never projected: a burst of work is not a
    /// pace that holds for days.
    #[test]
    fn the_week_is_neither_fused_nor_projected() {
        let now = 100_000;
        let reading = VendorLimits {
            observed_at: Some(now),
            ..from_claude(
                &Ok(UsageSnapshot {
                    five_hour: None,
                    seven_day: Some(window(60.0, Some(now + 3 * 86_400))),
                }),
                now,
            )
        };
        let history = [PreviousReading {
            at: now - 20 * 60,
            windows: vec![(Some(CLAUDE_WEEKLY_MINUTES), 29.0)],
        }];
        assert!(burn_check(&reading, &history).is_none());
        assert_eq!(
            forecast(
                60.0,
                now,
                Some(now + 3 * 86_400),
                Some((29.0, now - 20 * 60)),
                Some(now),
                Some(CLAUDE_WEEKLY_MINUTES)
            ),
            Forecast::NoRate
        );
    }

    /// The anchor is the newest reading a quarter of an hour back, so a quick
    /// second read never becomes the other end of the slope.
    #[test]
    fn the_anchor_is_the_newest_reading_a_quarter_hour_back() {
        let history = [
            history_at(1_000, 5.0),
            history_at(1_000 + 20 * 60, 8.0),
            history_at(1_000 + 30 * 60, 9.0),
        ];
        let now = 1_000 + 35 * 60;
        assert_eq!(
            anchor_in(&history, now).map(|r| r.at),
            Some(1_000 + 20 * 60)
        );
        assert_eq!(anchor_in(&history, 1_000 + 60), None);
    }

    /// A fast climb between two reads quickens the poll before the fuse has
    /// enough to speak; the week climbing does not.
    #[test]
    fn a_fast_climb_quickens_the_poll() {
        assert!(climbing_fast(&[history_at(0, 10.0), history_at(600, 16.0)]));
        assert!(!climbing_fast(&[
            history_at(0, 10.0),
            history_at(600, 12.0)
        ]));
        assert!(!climbing_fast(&[history_at(0, 10.0)]));
    }

    /// History forgets what is too old to measure against.
    #[test]
    fn history_keeps_three_hours() {
        let mut history = vec![history_at(0, 1.0)];
        remember(&mut history, history_at(4 * 3600, 2.0), 4 * 3600);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].at, 4 * 3600);
    }

    /// The notification says what is happening, by how much, and who.
    #[test]
    fn the_burn_notification_names_the_window_the_pace_and_the_session() {
        let now = 100_000;
        let mut burn = BurnAlarm {
            window: FIVE_HOURS,
            resets_at: now + 4 * 3600,
            utilization: 40.0,
            points: 30.0,
            span: 20 * 60,
            runs_out_at: now + 40 * 60,
            culprit: None,
        };
        let (summary, body) = burn_notification_text(Vendor::Claude, &burn, now);
        assert_eq!(summary, "Claude 5-hour limit is burning fast");
        assert_eq!(
            body,
            format!(
                "30% gone in the last 20m \u{2014} at this pace it runs out at {} (resets {}). \
                                  Not traced to a Splitlane session.",
                clock_hhmm((now + 40 * 60) as u64 * 1000),
                clock_hhmm((now + 4 * 3600) as u64 * 1000)
            )
        );
        burn.culprit = Some(Culprit {
            thread_id: 7,
            title: "refactor auth".to_string(),
            project: "atlas".to_string(),
        });
        let (_, body) = burn_notification_text(Vendor::Claude, &burn, now);
        assert!(
            body.ends_with("Busiest here: \u{ab}refactor auth\u{bb} in atlas."),
            "{body}"
        );
    }

    /// The culprit is whoever spent most since the burn began, from each
    /// session's own transcript; a burn nobody here spent names nobody.
    #[test]
    fn the_culprit_is_whoever_spent_most_in_the_span() {
        let dir = std::env::temp_dir().join(format!("splitlane-burn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let line = |id: &str, at: &str, output: u64| {
            format!(
                r#"{{"type":"assistant","timestamp":"{at}","message":{{"id":"{id}","model":"claude-sonnet-4-5","usage":{{"input_tokens":10,"output_tokens":{output}}}}}}}"#
            )
        };
        let busy = dir.join("busy.jsonl");
        let calm = dir.join("calm.jsonl");
        std::fs::write(
            &busy,
            format!(
                "\n{}\n{}\n",
                line("a", "2026-09-18T10:00:00Z", 5_000),
                line("b", "2026-09-18T10:05:00Z", 90_000)
            ),
        )
        .unwrap();
        std::fs::write(
            &calm,
            format!("\n{}\n", line("c", "2026-09-18T10:06:00Z", 800)),
        )
        .unwrap();
        let since = chrono::DateTime::parse_from_rfc3339("2026-09-18T10:01:00Z")
            .unwrap()
            .timestamp();
        let culprit = find_culprit(
            vec![
                (1, "calm".into(), "atlas".into(), calm.clone()),
                (2, "busy".into(), "atlas".into(), busy.clone()),
            ],
            since,
        )
        .expect("both spent in the span");
        assert_eq!(culprit.thread_id, 2);
        // Nothing after the span began: nobody here is burning it.
        assert!(
            find_culprit(
                vec![(1, "calm".into(), "atlas".into(), calm)],
                since + 3_600
            )
            .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reply carrying only the week must not be rendered as a session at 0%.
    #[test]
    fn a_missing_chosen_window_degrades_rather_than_inventing_a_zero() {
        let limits = read(None, Some(window(21.0, None)));
        let copy = footer_copy(&limits, None, &[], Some(FIVE_HOURS), 1_000);
        // The window the row was showing is not in this reply, so the row
        // shows the one that is rather than a session at nothing.
        assert_eq!(copy.label, "Claude weekly limit");
        assert_eq!(copy.value, "21%");
        assert_eq!(copy.other_label, "");
    }
}
