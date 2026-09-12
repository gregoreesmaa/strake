//! The [`ScriptEngine`] seam (issue #4, Phase 0).
//!
//! The Boa backend implements this trait today; the Phase-1 QuickJS-ng
//! migration implements the same trait behind the same conformance suite
//! (`tests/event_loop.rs`), so embedders (`ScriptDocument`, the WPT runner,
//! …) never touch engine specifics. Only engine-agnostic behavior belongs
//! here: evaluation, microtask checkpoints, timers, and deadlines.

use url::Url;
use web_time::Instant;

/// JavaScript engine backend: script evaluation with microtask checkpoints
/// and timer firing, per the HTML event-loop model.
///
/// All methods capture uncaught errors into the document's error sink rather
/// than propagating them; embedders drain them explicitly. Object-safe so
/// embedders can hold `&mut dyn ScriptEngine`.
pub trait ScriptEngine {
    /// Evaluate a classic script, then drain the microtask queue.
    fn eval(&mut self, code: &str, description: &str);

    /// Evaluate an ES module script (parse, link against `url` for imports,
    /// evaluate), then drain the microtask queue.
    fn eval_module(&mut self, code: &str, url: Option<&Url>);

    /// Microtask checkpoint: run pending promise jobs until empty.
    fn run_jobs(&mut self, description: &str);

    /// Run all currently-due timers, one microtask checkpoint per timer task.
    /// Returns whether any JavaScript ran.
    fn run_due_timers(&mut self) -> bool;

    /// Deadline of the soonest pending timer, if any.
    fn next_timer_deadline(&self) -> Option<Instant>;
}
