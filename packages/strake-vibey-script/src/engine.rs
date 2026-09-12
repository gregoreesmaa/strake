//! The [`ScriptEngine`] seam (issue #4, Phase 0).
//!
//! The Boa backend implements this trait today; the Phase-1 QuickJS-ng
//! migration implements the same trait behind the same conformance suite
//! (`tests/event_loop.rs`), so embedders drive evaluation, microtask
//! checkpoints, and timers through this trait without touching engine
//! specifics. Only engine-agnostic behavior belongs here: evaluation,
//! microtask checkpoints, timers, and deadlines.
//!
//! Scope note (PR #78): event-listener dispatch is NOT behind this seam in
//! Phase 0. `dispatch_dom_event` / `dispatch_document_event` /
//! `dispatch_window_event` (runtime.rs) invoke the concrete `run_jobs`, and
//! `ScriptEventHandler` (event_handler.rs) holds `&mut ScriptRuntime`
//! directly, so a mock `ScriptEngine` cannot observe or replace event
//! microtasks today. A backend swap must port the event path (dispatch,
//! `sync_named_element_globals`, ready-state setters, direct `ctx.state`
//! accesses) along with this trait.

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
