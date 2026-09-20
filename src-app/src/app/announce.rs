//! One movement, on the edge of the attention queue.
//!
//! The rule this exists for, taken word for word from the design: **the ephemeral
//! channel announces, the persistent one remembers, and neither may be the only
//! one.** This app had only the second. The dot, the chip and the rail row are
//! always true and never an event, so a session that goes quiet while the
//! reader is looking at a different pane changes nothing anywhere in their
//! field of view - and that reader, in the app but looking elsewhere, is the
//! ordinary case rather than the edge one.
//!
//! # The edge, which is a property of the queue and not of the person
//!
//! > One signal at the moment the attention queue goes from "nobody is waiting
//! > for you" to "somebody is". While anybody is already in it - silence.
//!
//! Two limits keep it from being armed by accident, and this module has both
//! because the queue's own emptiness is the only input:
//!
//! - **Coming back to the desk is not an edge.** Nor is the machine waking, nor
//!   the window taking focus, nor a repaint, a theme change, a rail reorder or
//!   a filter. None of those change [`ActivityCounts`], so none of them can
//!   reach this.
//! - **A top-up is not an edge.** Three in the queue, two dealt with, a fourth
//!   arrives: the queue never emptied, so nothing fires. Only `0 -> >=1`.
//!
//! Emptying the queue is deliberately silent, and the argument is stronger than
//! "it needs no action": the queue emptied **because you emptied it**. A signal
//! about a change you just caused is the definition of noise, and you already
//! have your own receipt for it. The asymmetry is on purpose.
//!
//! # Why movement and not sound
//!
//! The complaint splits into three audiences and only the third wants a sound:
//! somebody in the app looking at another pane is served by **peripheral vision
//! catching movement**, since the chip is permanently on screen; somebody who
//! has left the machine is served by the sticky mark, which already works; only
//! somebody sitting in front of the machine and not looking at the display
//! needs a noise. The case found in use was the first. Sound stays a legitimate
//! future on this same edge, and inherits two properties from "only the third
//! case": off by default, and **never the only announcement** - a product whose
//! finishes are inaudible with sound off has to stay visible.
//!
//! # The trap, and it is the reason this is state and not a mount
//!
//! The pulse is bound to a **state transition**, never to an element appearing.
//! Reordering the rail remounts its rows; an animation keyed on mounting would
//! replay the whole queue's worth of pulses every time the order changed. So
//! the app records the edge here, the drawing asks whether *this* dot is in
//! *this* announcement, and the announcement is taken down by a timer.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Hsla, IntoElement, ParentElement, SharedString, Styled, div, px};

use crate::SplitlaneApp;
use crate::ui_tokens as tok;

/// The log target for the edge, mirroring `splitlane::agent_state`.
pub(crate) const TRACE: &str = "splitlane::announce";

/// How long an announcement stays on the app after it fires.
///
/// A shade past [`tok::motion::ANNOUNCE`] on purpose: GPUI stops asking for
/// frames the moment a one-shot animation is done, so the last frame of the
/// pulse can be the last frame for a while. The takedown has to come from a
/// timer rather than from the frame loop, and it has to come *after* the
/// movement rather than during it.
const ANNOUNCE_TAKEDOWN: Duration = Duration::from_millis(520);

/// How long a hole in the frame loop has to be before the queue counts as
/// unobserved.
///
/// **This is a measured number, not a chosen one.** Traced live on 7 September:
/// with the window on screen the loop runs this function every **~533 ms**,
/// dead regular, whether or not the window is active - a mounted terminal's
/// cursor blink keeps it going. With the window hidden it ran **zero times in
/// twelve seconds**. The two states are that far apart, so anything between
/// them separates them; two seconds is ~4x the observed cadence, and it is also
/// one whole `agent_state_pass` interval, which is the other way of saying it -
/// a hole this long means a pass could have moved the queue with nobody
/// watching.
///
/// Erring short costs a missed announcement, erring long costs a false one, and
/// only the first is allowed - so this is deliberately nearer the cadence than
/// the absence. The residual, named rather than hidden: the ~533 ms depends on
/// a terminal painting somewhere on screen, and a visible window with no
/// mounted terminal in the active container paints rarely, so a real edge there
/// can be adopted instead of announced. That is the losing direction, which the
/// invariant permits.
const ATTENTION_EDGE_UNOBSERVED: Duration = Duration::from_secs(2);

/// Whether the frame loop was away long enough that the queue could have moved
/// with nobody able to see it.
///
/// A pure rule, extracted so it is unit-tested without a running app - the same
/// shape `ipc_handler::send_text_gate_open` uses, and for the same reason; the
/// behaviour built on it was checked live instead.
///
/// `None` is the first frame, which adopts: an app that comes up with an agent
/// already waiting has not just been handed one.
fn queue_was_unobserved(last_seen: Option<Instant>, now: Instant) -> bool {
    last_seen.is_none_or(|last| now.saturating_duration_since(last) > ATTENTION_EDGE_UNOBSERVED)
}

/// Where the rise ends and the settle begins, as a fraction of
/// [`tok::motion::ANNOUNCE`]. 140 ms of 480.
const RISE_END: f32 = 140. / 480.;
/// Where the settle ends. 420 ms of 480 - the dot is home before the halo is,
/// which is what makes the halo read as the thing that was thrown off.
const SETTLE_END: f32 = 420. / 480.;

/// How far the dot grows. **Area is the payload**: peripheral vision integrates
/// a change of area far better than it resolves a 6px shape, so the halo does
/// the work and the dot's growth is there to make the halo look caused rather
/// than decorative.
const DOT_PEAK_SCALE: f32 = 1.9;
/// How far the halo spreads before it is gone.
const HALO_SPREAD: f32 = 10.;
/// The halo's opacity at the moment it leaves the dot.
const HALO_OPACITY: f32 = 0.45;

/// Where an announcement has got to, handed to whatever draws a dot.
///
/// It carries the **edge's own instant** rather than a progress number so the
/// drawing can ask at paint time: a dot laid out one frame later than another
/// must not start its own 480 ms, it must join the one in flight.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnnouncePhase {
    /// Bumped once per edge, and what keys the animation's element id - so a
    /// second edge replaces the first rather than continuing it.
    generation: u64,
    started: Instant,
}

impl AnnouncePhase {
    /// How far through [`tok::motion::ANNOUNCE`] this announcement is, clamped.
    fn delta(&self) -> f32 {
        (self.started.elapsed().as_secs_f32() / tok::motion::ANNOUNCE.as_secs_f32()).clamp(0., 1.)
    }
}

/// One announcement in flight.
#[derive(Debug, Clone)]
pub(crate) struct Announcement {
    /// Bumped once per edge. It keys the animation's element id, which is what
    /// makes GPUI start the movement over rather than continue a previous one.
    pub(crate) generation: u64,
    /// When the edge happened.
    started: Instant,
    /// The surfaces that entered the queue at the edge. **Their** rail dots
    /// pulse; the chip pulses once whatever the number, because the edge is one
    /// edge however many sessions made it.
    threads: HashSet<u64>,
}

impl Announcement {
    fn phase(&self) -> AnnouncePhase {
        AnnouncePhase {
            generation: self.generation,
            started: self.started,
        }
    }
}

/// A dot's fill, and what the shape means.
///
/// **Shape carries the kind, tone carries the rank** - that is the split, and
/// the reason the halo may take the accent in every rank without claiming one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DotFill {
    /// Filled: the session is standing by itself.
    Solid(Hsla),
    /// A ring: there is something here to accept.
    ///
    /// The border is 1.5px and that is a floor, not a taste - it is the first
    /// thing rounding eats, and this is a native app with terminal fonts where
    /// 6px is not the browser's 6px. `starting` has drawn a ring at this size
    /// since it shipped, which is the evidence that the shape reads at all; if it
    /// ever stops, the designer's own fallback is size rather than fill - a
    /// filled 6px against a filled 4px, worse but legible under any renderer.
    Ring(Hsla),
}

/// One dot, with the announcement's movement when this dot is the one being
/// announced.
///
/// `halo` is the accent, always, and `announcing` is the generation to key the
/// movement on. Two things follow from the halo taking the accent in **both**
/// ranks. The dot is a state and carries the rank; the halo is an event and
/// carries none - one act, performed identically whether a question arrived or
/// a result did, with the rank left in what stays on screen afterwards. And it
/// is only legal because **the halo ends at zero**: the accent means "something
/// wants you", and a mark that lives 480 ms and leaves nothing behind makes no
/// standing claim. Let it settle on any non-zero value and the argument
/// collapses, which is why "ends at zero" is load-bearing rather than tidy.
pub(crate) fn announceable_dot(
    id: SharedString,
    size: f32,
    fill: DotFill,
    halo: Hsla,
    announcing: Option<AnnouncePhase>,
) -> AnyElement {
    let dot = move |scale: f32| {
        let grown = size * scale;
        let inset = (size - grown) / 2.;
        let base = div()
            .absolute()
            .top(px(inset))
            .left(px(inset))
            .size(px(grown))
            .rounded_full();
        match fill {
            DotFill::Solid(color) => base.bg(color),
            DotFill::Ring(color) => base.border(px(1.5)).border_color(color),
        }
    };
    let Some(phase) = announcing else {
        // At rest there is no wrapper at all, which is the other half of "ends
        // at zero": the resting pixels are the ones that were there before the
        // announcement, not a spent animation holding still.
        return match fill {
            DotFill::Solid(color) => div().flex_none().size(px(size)).rounded_full().bg(color),
            DotFill::Ring(color) => div()
                .flex_none()
                .size(px(size))
                .rounded_full()
                .border(px(1.5))
                .border_color(color),
        }
        .into_any_element();
    };
    use gpui::AnimationExt;
    // The box keeps the dot's resting footprint and the growth happens inside
    // it, absolutely positioned and inset to stay centred - so a 6px dot
    // reaching 11.4px moves nothing beside it. The **id keys the movement to
    // the edge**: a new generation is a new element state, which is what makes
    // GPUI start the pulse rather than continue a previous one.
    div()
        .flex_none()
        .size(px(size))
        .relative()
        .with_animation(
            SharedString::from(format!("{id}-announce-{}", phase.generation)),
            gpui::Animation::new(tok::motion::ANNOUNCE),
            move |container, _| {
                // **The animation is the frame pump; the announcement is the
                // clock.** GPUI's own delta restarts from zero whenever the
                // element state is new - which happens on a remount, and the
                // rail remounts rows. Keyed on GPUI's clock, a row scrolled out
                // and back inside the 480 ms would play the pulse a second time
                // from the top, and a dot mounted late would start a fresh one
                // out of phase with the chip's. Read off the edge's own
                // instant, a remount resumes where the announcement actually
                // is, and a late mount joins it rather than beginning it.
                let (scale, halo_progress) = pulse_at(phase.delta());
                container.child(dot(scale).shadow(vec![
                    gpui::BoxShadow::new(
                        px(0.),
                        px(0.),
                        halo.opacity(HALO_OPACITY * (1. - halo_progress)),
                    )
                    .spread_radius(px(HALO_SPREAD * halo_progress)),
                ]))
            },
        )
        .into_any_element()
}

/// The pulse at a point in its 480 ms: the dot's scale, and how far the halo
/// has travelled.
///
/// Pure, so the shape can be asserted rather than watched. The two curves are
/// [`tok::motion::ANNOUNCE_RISE`] and [`tok::motion::ANNOUNCE_SETTLE`]; the
/// halo is linear in both spread and opacity across the whole duration, so it
/// is still leaving when the dot is already home.
fn pulse_at(delta: f32) -> (f32, f32) {
    let delta = delta.clamp(0., 1.);
    let scale = if delta <= RISE_END {
        1. + (DOT_PEAK_SCALE - 1.) * tok::motion::ANNOUNCE_RISE.ease(delta / RISE_END)
    } else if delta <= SETTLE_END {
        DOT_PEAK_SCALE
            - (DOT_PEAK_SCALE - 1.)
                * tok::motion::ANNOUNCE_SETTLE.ease((delta - RISE_END) / (SETTLE_END - RISE_END))
    } else {
        1.
    };
    (scale, delta)
}

impl SplitlaneApp {
    /// Look at the queue and fire if it has just gone from empty to not.
    ///
    /// Once a frame, from `sync_surface_facts`, and **after** the frame's own
    /// clearing of marks on screen: a run that ended under the reader's eyes is
    /// never marked, so it never enters the queue and never announces.
    /// Trace this with `RUST_LOG=splitlane::announce=debug`.
    ///
    /// A one-shot 480 ms movement leaves nothing behind to look at afterwards,
    /// so without a line here the only way to check that it fired on an edge -
    /// and, which matters more, that it stayed silent on everything that is not
    /// one - is to be watching the pixels at the moment it happened. The
    /// detector has the same problem and the same answer
    /// (`splitlane::agent_state`).
    pub(crate) fn refresh_attention_edge(&mut self, cx: &mut Context<Self>) {
        let counts = self.activity_counts(cx);
        self.activity_counts_cache = counts;
        let empty = counts.is_empty();
        // **Has this loop been running?** The edge is observed here and nowhere
        // else, so a hole in the frame loop is a stretch of time in which the
        // queue could have filled with nobody able to see it - and the first
        // frame back would otherwise compare a latch from before the hole
        // against a queue filled during it, and announce. That is the failure
        // the design forbids outright, and it was live: hiding the app and
        // showing it again **without focusing it** announced on return,
        // reproduced twice on 7 September.
        //
        // Asking the window instead of the loop does not work, and the attempt
        // is worth recording. `observe_window_activation` was already the hook,
        // and it is both too narrow and too late: painting resumes on becoming
        // *visible*, which is a different event from becoming *active*, and
        // GPUI has no visibility or occlusion observer at this revision.
        // Dropping the latch on the way out fails for a subtler reason - that
        // observer ends in `cx.notify()`, so it schedules a frame of its own
        // which consumes the request while the window is still up, re-arming
        // the latch to "empty" just before the app disappears.
        //
        // The loop's own discontinuity has none of those problems: it needs no
        // platform signal, and it covers hiding, minimising, occlusion, another
        // Space and a sleeping machine without naming any of them.
        let now = Instant::now();
        let unobserved = queue_was_unobserved(self.attention_edge_last_seen, now);
        self.attention_edge_last_seen = Some(now);
        // The first frame adopts whatever it finds without firing: showing the
        // window is not an edge, and an app that comes up with an agent already
        // waiting has not just been handed one. `rebaseline_attention_edge`
        // asks for the same treatment whenever the app has stopped watching.
        if self.attention_edge_needs_rebaseline || unobserved {
            log::debug!(
                target: TRACE,
                "adopting the queue without announcing (empty={empty}, unobserved={unobserved}): \
                 {counts:?}"
            );
            self.attention_edge_needs_rebaseline = false;
            self.attention_queue_was_empty = empty;
            return;
        }
        if self.attention_queue_was_empty && !empty {
            log::debug!(target: TRACE, "edge 0 -> {counts:?}: announcing once");
            self.announce_generation = self.announce_generation.wrapping_add(1);
            let generation = self.announce_generation;
            self.announcement = Some(Announcement {
                generation,
                started: Instant::now(),
                threads: self.activity_member_threads(cx),
            });
            cx.spawn(async move |this, cx| {
                smol::Timer::after(ANNOUNCE_TAKEDOWN).await;
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        if app
                            .announcement
                            .as_ref()
                            .is_some_and(|live| live.generation == generation)
                        {
                            app.announcement = None;
                            cx.notify();
                        }
                    })
                });
            })
            .detach();
            cx.notify();
        }
        self.attention_queue_was_empty = empty;
        // A belt for the timer's braces: a session that lost its window while
        // the pulse was in flight comes back with nothing held over.
        if self
            .announcement
            .as_ref()
            .is_some_and(|live| live.started.elapsed() >= ANNOUNCE_TAKEDOWN)
        {
            self.announcement = None;
        }
    }

    /// Forget what the edge thought the queue looked like, without announcing.
    ///
    /// **The edge is observed by the frame loop, and the frame loop stops.** A
    /// minimised window, a fully occluded one, a sleeping machine: nothing
    /// paints, so `refresh_attention_edge` does not run, and the latch keeps
    /// whatever it held before the absence. Meanwhile the state pass and the
    /// hook handler keep marking finished runs on their own timers. Without
    /// this, the first frame after the window comes back compares a latch from
    /// *before* the absence against a queue filled *during* it and announces -
    /// which is the failure the design names outright:
    ///
    /// > The returning reader gets only the marks. They missed the
    /// > announcement because announcements are for people who are present.
    /// > **The failure mode to avoid is the opposite one:** playing the
    /// > announcement on return, so that stepping away costs somebody a noise
    /// > they cannot act on any faster for.
    ///
    /// Adopting rather than firing is the same rule the first frame already
    /// applies, and it says what the app actually knows: the queue is different
    /// now, and **when** it changed is not something an unobserved period can
    /// answer. An edge nobody could have seen is not an edge this app may
    /// claim.
    pub(crate) fn rebaseline_attention_edge(&mut self) {
        self.attention_edge_needs_rebaseline = true;
    }

    /// The chip's counts as this frame measured them.
    ///
    /// Cached rather than re-derived, because **both** window frames draw the
    /// chip and only one of them is on screen: without this, the same walk of
    /// every pane in every container ran once for the title bar and again for
    /// the toolbar. Written by `refresh_attention_edge` at the top of the
    /// frame, read below it, so it is never a frame behind.
    pub(crate) fn activity(&self) -> crate::app::waiting::ActivityCounts {
        self.activity_counts_cache
    }

    /// The **chip's** movement, or `None` at rest.
    ///
    /// One per edge whatever the number of sessions: eight finishes in one tick
    /// are one edge and one pulse.
    pub(crate) fn chip_announcement(&self) -> Option<AnnouncePhase> {
        self.announcement.as_ref().map(Announcement::phase)
    }

    /// **This session's** rail dot movement, or `None`.
    ///
    /// If two sessions make the edge together, both their glyphs pulse and the
    /// chip still pulses once: both arrived, the edge is still one edge.
    pub(crate) fn thread_announcement(&self, thread_id: u64) -> Option<AnnouncePhase> {
        self.announcement
            .as_ref()
            .filter(|live| live.threads.contains(&thread_id))
            .map(Announcement::phase)
    }

    /// Which surfaces are in the queue right now, by surface-record id.
    ///
    /// Off the popover's own rows, like the counts - so what pulses in the rail
    /// is exactly what the list would show, and a session that is two things at
    /// once is one member. A pane session with no surface record contributes
    /// nothing here, because the thing that pulses is a **rail row** and it has
    /// none.
    fn activity_member_threads(&self, cx: &Context<Self>) -> HashSet<u64> {
        self.attention_queue_rows(cx)
            .into_iter()
            .filter_map(|row| row.thread_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape, asserted rather than watched: a 5px excursion on a 6px dot,
    /// once, is not something an eye can check on screen.
    #[test]
    fn the_pulse_starts_and_ends_at_rest() {
        let (scale, halo) = pulse_at(0.);
        assert!((scale - 1.).abs() < 1e-3, "the dot starts at rest");
        assert!(halo.abs() < 1e-3, "the halo starts on the dot");

        let (scale, halo) = pulse_at(1.);
        assert!((scale - 1.).abs() < 1e-3, "the dot ends at rest");
        // **Load-bearing, not tidy**: the accent is legal on the
        // halo only because the halo ends at nothing. Any residue and the
        // announcement would be making a standing claim.
        assert!(
            (halo - 1.).abs() < 1e-3,
            "the halo has fully travelled, so its opacity is zero"
        );
    }

    #[test]
    fn the_dot_peaks_where_the_rise_ends_and_is_home_before_the_halo() {
        let (peak, _) = pulse_at(RISE_END);
        assert!(
            (peak - DOT_PEAK_SCALE).abs() < 1e-3,
            "the rise ends at the peak"
        );
        let (settled, halo) = pulse_at(SETTLE_END);
        assert!((settled - 1.).abs() < 1e-3, "the dot is home at 420 ms");
        assert!(halo < 1., "and the halo is still leaving");
    }

    /// A remount inside the flight window must **resume** the pulse, not
    /// restart it - which is the whole reason the phase is read off the edge's
    /// own instant instead of GPUI's element clock. The rail remounts rows.
    #[test]
    fn the_phase_is_the_edge_s_own_clock_so_a_remount_resumes() {
        let started = Instant::now() - Duration::from_millis(240);
        let phase = AnnouncePhase {
            generation: 1,
            started,
        };
        let delta = phase.delta();
        assert!(
            (0.4..0.75).contains(&delta),
            "half-way through 480 ms, not back at the start: {delta}"
        );

        // And an announcement older than its own duration is finished rather
        // than looping: a dot mounted late joins at rest, it does not open a
        // second 480 ms of its own.
        let stale = AnnouncePhase {
            generation: 1,
            started: Instant::now() - Duration::from_millis(2_000),
        };
        assert_eq!(stale.delta(), 1.0);
        let (scale, halo) = pulse_at(stale.delta());
        assert!((scale - 1.).abs() < 1e-3);
        assert!((halo - 1.).abs() < 1e-3, "the halo has gone entirely");
    }

    /// One beat, not two: the scale never turns back up after it has settled.
    #[test]
    fn the_movement_is_one_beat() {
        let mut rising = true;
        let mut previous = pulse_at(0.).0;
        for step in 1..=480 {
            let scale = pulse_at(step as f32 / 480.).0;
            if rising && scale < previous - 1e-4 {
                rising = false;
            }
            assert!(
                rising || scale <= previous + 1e-4,
                "the dot grew again at {step} ms"
            );
            previous = scale;
        }
        assert!(!rising, "the dot came back down");
    }

    /// The frame loop is the only thing that observes the edge, so a hole in it
    /// is a stretch of time in which the queue could have filled with nobody
    /// able to see it - and the first frame back must adopt rather than
    /// announce, which is what the design forbids getting wrong.
    ///
    /// The numbers are the ones traced live on 7 September: with the window on
    /// screen this runs every ~533 ms, and with it hidden it did not run once
    /// in twelve seconds.
    #[test]
    fn a_hole_in_the_frame_loop_means_nobody_could_have_seen_the_queue() {
        let now = Instant::now();

        // The first frame has nothing to compare against and adopts.
        assert!(queue_was_unobserved(None, now));

        // The measured cadence with the window on screen, and a slow multiple
        // of it: both are the loop running, so an edge here is real and must
        // still be announced.
        for gap in [16, 533, 1_000, 1_900] {
            assert!(
                !queue_was_unobserved(now.checked_sub(Duration::from_millis(gap)), now),
                "a {gap} ms gap is the loop running, not a hole in it"
            );
        }

        // Anything past the threshold is a stretch the app did not paint
        // through: hidden, minimised, occluded, another Space, asleep.
        for gap in [3, 12, 600] {
            assert!(
                queue_was_unobserved(now.checked_sub(Duration::from_secs(gap)), now),
                "a {gap} s hole is time nobody could have been shown anything"
            );
        }
    }
}
