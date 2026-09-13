//! Timer support (`setTimeout` / `setInterval` / `requestAnimationFrame`)

use std::collections::HashSet;

use boa_engine::JsValue;
use boa_engine::object::JsObject;
use web_time::{Duration, Instant};

pub(crate) struct Timer {
    pub id: u64,
    pub deadline: Instant,
    /// `Some` for `setInterval` timers, which reschedule themselves.
    pub interval: Option<Duration>,
    pub callback: JsObject,
    pub args: Vec<JsValue>,
    /// HTML timer-task nesting level: 0 when scheduled outside a timer
    /// callback, otherwise the firing timer's level + 1. Levels above
    /// [`NESTING_CLAMP_LEVEL`] clamp the delay to
    /// [`NESTING_CLAMP_DELAY`](issue #6 acceptance).
    pub nesting: u32,
}

/// Nesting level above which `setTimeout`/`setInterval` delays clamp.
pub(crate) const NESTING_CLAMP_LEVEL: u32 = 5;
/// Minimum delay for deeply nested timers (HTML5, issue #6).
pub(crate) const NESTING_CLAMP_DELAY: Duration = Duration::from_millis(4);

/// Apply the HTML5 nesting clamp: timers scheduled from a timer task nested
/// deeper than [`NESTING_CLAMP_LEVEL`] run no sooner than
/// [`NESTING_CLAMP_DELAY`] out.
pub(crate) fn clamp_delay(nesting: u32, delay: Duration) -> Duration {
    if nesting > NESTING_CLAMP_LEVEL {
        delay.max(NESTING_CLAMP_DELAY)
    } else {
        delay
    }
}

#[derive(Default)]
pub(crate) struct TimerQueue {
    next_id: u64,
    timers: Vec<Timer>,
    /// Ids cancelled via `clearTimeout`/`clearInterval`/`cancelAnimationFrame`.
    ///
    /// `take_due` snapshots the due batch up front, so a timer cancelled from
    /// an earlier callback in the same batch is no longer in `timers` for
    /// `remove` to find. `run_due_timers` therefore consults this set at fire
    /// time (via [`take_cancelled`](Self::take_cancelled)) before invoking
    /// each due timer (issue #4, PR #78).
    cancelled: HashSet<u64>,
}

impl TimerQueue {
    pub fn add(
        &mut self,
        now: Instant,
        delay: Duration,
        interval: Option<Duration>,
        callback: JsObject,
        args: Vec<JsValue>,
        nesting: u32,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.timers.push(Timer {
            id,
            deadline: now + delay,
            interval,
            callback,
            args,
            nesting,
        });
        id
    }

    pub fn remove(&mut self, id: u64) {
        self.timers.retain(|timer| timer.id != id);
        // Also record the cancellation for timers already extracted by
        // `take_due`: the fire-time check in `run_due_timers` consults this.
        // Stale marks (ids neither queued nor in the current batch) are
        // pruned by `take_due`, so the set stays bounded.
        self.cancelled.insert(id);
    }

    /// Fire-time cancellation check for one due timer: returns `true` (and
    /// consumes the mark) if the id was cancelled after `take_due` extracted
    /// it — e.g. by `clearTimeout` from an earlier same-batch callback.
    pub fn take_cancelled(&mut self, id: u64) -> bool {
        self.cancelled.remove(&id)
    }

    /// The deadline of the timer which is due soonest (if any)
    pub fn next_deadline(&self) -> Option<Instant> {
        self.timers.iter().map(|timer| timer.deadline).min()
    }

    /// Remove and return all timers that are due at `now`, soonest first.
    /// Interval timers are rescheduled.
    ///
    /// Interval rescheduling is anchored to poll time (`now + interval`), not
    /// to the missed deadline: a late `poll` shifts the interval phase rather
    /// than producing catch-up ticks (pinned by
    /// `interval_reschedule_anchors_to_poll_time` in `tests/event_loop.rs`).
    ///
    /// The returned batch is a snapshot: timers scheduled from inside a
    /// callback (including zero-delay nested timeouts, whose deadline is
    /// `now`) wait for the next `poll`, after already-queued same-deadline
    /// siblings (pinned by `nested_zero_delay_timer_waits_for_next_poll`).
    pub fn take_due(&mut self, now: Instant) -> Vec<Timer> {
        let mut due: Vec<Timer> = Vec::new();
        let mut idx = 0;
        while idx < self.timers.len() {
            if self.timers[idx].deadline <= now {
                due.push(self.timers.swap_remove(idx));
            } else {
                idx += 1;
            }
        }

        // Reschedule interval timers (keeping the firing timer's nesting
        // level, so an interval created by a deeply nested task stays
        // clamped like the HTML `timeout()` steps require).
        for timer in &due {
            if let Some(interval) = timer.interval {
                self.timers.push(Timer {
                    id: timer.id,
                    deadline: now + interval.max(Duration::from_millis(1)),
                    interval: timer.interval,
                    callback: timer.callback.clone(),
                    args: timer.args.clone(),
                    nesting: timer.nesting,
                });
            }
        }

        // Drop stale cancellation marks: a mark implies the id was removed
        // from the queue, so no id extracted into `due` above can be marked
        // yet (ids are never reused, and interval reschedules keep the id of
        // their unmarked `due` entry). Same-batch cancels mark their ids
        // after this point, and those marks are consumed at fire time by
        // `take_cancelled`. Retaining only still-queued ids keeps the set
        // bounded across polls that never touch those ids again.
        self.cancelled
            .retain(|id| self.timers.iter().any(|timer| timer.id == *id));

        // Order by deadline, breaking ties by schedule order (timer ids grow
        // monotonically). `swap_remove` above scrambles arrival order, and a
        // deadline-only stable sort would preserve that scramble for timers
        // sharing a deadline (issue #4).
        due.sort_by_key(|timer| (timer.deadline, timer.id));
        due
    }
}
