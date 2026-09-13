//! Phase-0 event-loop conformance (issue #4): microtask checkpoint ordering,
//! timer FIFO, cancellation, intervals, rAF, and error capture on the Boa
//! engine. These are the WPT timers/microtask-subset behaviors Phase 0 must
//! hold before any engine swap. Virtual time: no sleeping, fully deterministic.

use strake_dom::{Document, DocumentConfig};
use strake_vibey_script::ScriptDocument;

fn doc_with_script(script: &str) -> ScriptDocument {
    let html = format!("<html><body><script>{script}</script></body></html>");
    ScriptDocument::from_html(&html, DocumentConfig::default())
        .without_timer_thread()
        .with_virtual_time()
}

/// Execute scripts, then jump the virtual clock deadline-to-deadline,
/// polling timers until none remain.
fn drain_timers(doc: &mut ScriptDocument) {
    doc.execute_scripts();
    while let Some(deadline) = doc.next_timer_deadline() {
        doc.advance_clock_to(deadline);
        doc.poll(None);
    }
}

#[test]
fn microtasks_drain_before_timers() {
    let mut doc = doc_with_script(
        r#"
        queueMicrotask(() => __strake_send_message("micro"));
        setTimeout(() => __strake_send_message("timer"), 0);
        "#,
    );
    doc.execute_scripts();
    assert_eq!(doc.take_messages(), vec!["micro"]);
    drain_timers_continued(&mut doc);
    assert_eq!(doc.take_messages(), vec!["timer"]);
}

/// Continue a virtual-time drain when scripts were already executed.
fn drain_timers_continued(doc: &mut ScriptDocument) {
    while let Some(deadline) = doc.next_timer_deadline() {
        doc.advance_clock_to(deadline);
        doc.poll(None);
    }
}

#[test]
fn timer_callback_microtask_runs_before_next_timer() {
    // HTML spec: a microtask checkpoint runs after *each* task. A microtask
    // queued by the first timer must run before the second timer's callback.
    let mut doc = doc_with_script(
        r#"
        setTimeout(() => {
            __strake_send_message("t1");
            queueMicrotask(() => __strake_send_message("m1"));
        }, 0);
        setTimeout(() => __strake_send_message("t2"), 0);
        "#,
    );
    drain_timers(&mut doc);
    assert_eq!(doc.take_messages(), vec!["t1", "m1", "t2"]);
}

#[test]
fn equal_deadline_timers_fire_in_schedule_order() {
    let mut doc = doc_with_script(
        r#"
        setTimeout(() => __strake_send_message("t1"), 0);
        setTimeout(() => __strake_send_message("t2"), 0);
        setTimeout(() => __strake_send_message("t3"), 0);
        "#,
    );
    drain_timers(&mut doc);
    assert_eq!(doc.take_messages(), vec!["t1", "t2", "t3"]);
}

#[test]
fn clear_timeout_cancels() {
    let mut doc = doc_with_script(
        r#"
        const id = setTimeout(() => __strake_send_message("fired"), 0);
        clearTimeout(id);
        "#,
    );
    drain_timers(&mut doc);
    assert!(doc.take_messages().is_empty());
    assert_eq!(doc.next_timer_deadline(), None);
}

#[test]
fn interval_repeats_until_cleared() {
    let mut doc = doc_with_script(
        r#"
        let n = 0;
        const id = setInterval(() => {
            __strake_send_message("tick");
            if (++n === 3) clearInterval(id);
        }, 10);
        "#,
    );
    drain_timers(&mut doc);
    assert_eq!(doc.take_messages(), vec!["tick", "tick", "tick"]);
}

#[test]
fn timer_throw_is_captured_not_lost() {
    let mut doc = doc_with_script(r#"setTimeout(() => { throw new Error("boom"); }, 0);"#);
    drain_timers(&mut doc);
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 1, "one captured timer error, got {errors:?}");
    assert!(errors[0].contains("boom"), "got {:?}", errors[0]);
}

#[test]
fn raf_fires_with_numeric_timestamp() {
    let mut doc = doc_with_script(
        r#"requestAnimationFrame((ts) => __strake_send_message("raf:" + typeof ts));"#,
    );
    drain_timers(&mut doc);
    assert_eq!(doc.take_messages(), vec!["raf:number"]);
}

#[test]
fn timer_throw_still_runs_queued_microtask_and_next_timer() {
    // PR #78(a): the per-timer microtask checkpoint in `run_due_timers` sits
    // outside the callback-error branch, so a throwing timer must neither
    // swallow the microtask it queued nor stop the next timer.
    let mut doc = doc_with_script(
        r#"
        setTimeout(() => {
            __strake_send_message("t1");
            queueMicrotask(() => __strake_send_message("m1"));
            throw new Error("boom");
        }, 0);
        setTimeout(() => __strake_send_message("t2"), 0);
        "#,
    );
    drain_timers(&mut doc);
    assert_eq!(doc.take_messages(), vec!["t1", "m1", "t2"]);
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 1, "one captured timer error, got {errors:?}");
    assert!(errors[0].contains("boom"), "got {:?}", errors[0]);
}

#[test]
fn throwing_microtask_drops_later_siblings_known_gap() {
    // PR #78(b), scoped explicitly: Boa 0.22's `SimpleJobExecutor` bails out
    // of the whole promise-job batch on the first failure
    // (`boa_engine 0.22 src/job.rs`: `let jobs = mem::take(...promise_jobs);`
    // then `if let Err(err) = job.call(...) { self.clear(); return Err(err); }`),
    // so microtasks queued behind a thrower never run. Per the HTML
    // "perform a microtask checkpoint" loop `m1`'s error would be reported
    // and `m2` would still run; here `m2` vanishes. This test pins the
    // Phase-0 contract so the Phase-1 QuickJS-ng swap must make a deliberate
    // choice (continue-and-report-each vs. drop) instead of inheriting a
    // silent divergence.
    let mut doc = doc_with_script(
        r#"
        queueMicrotask(() => { __strake_send_message("m1"); throw new Error("micro-boom"); });
        queueMicrotask(() => __strake_send_message("m2"));
        "#,
    );
    doc.execute_scripts();
    assert_eq!(doc.take_messages(), vec!["m1"]);
    let errors = doc.take_js_errors();
    assert_eq!(
        errors.len(),
        1,
        "one captured microtask error, got {errors:?}"
    );
    assert!(errors[0].contains("micro-boom"), "got {:?}", errors[0]);
    // The dropped sibling is gone, not merely deferred: a further checkpoint
    // must not resurrect it.
    doc.poll(None);
    assert!(doc.take_messages().is_empty());
}

#[test]
fn clear_timeout_cancels_same_batch_sibling() {
    // PR #78(c): `t1` and `t2` share a deadline and are extracted as one
    // batch, but `clearTimeout` from `t1` must still prevent `t2` from
    // running (HTML: clearing a task that has not run yet cancels it).
    let mut doc = doc_with_script(
        r#"
        let id2;
        setTimeout(() => { clearTimeout(id2); __strake_send_message("t1"); }, 0);
        id2 = setTimeout(() => __strake_send_message("t2"), 0);
        "#,
    );
    drain_timers(&mut doc);
    assert_eq!(doc.take_messages(), vec!["t1"]);
    assert!(doc.take_js_errors().is_empty());
}

#[test]
fn clear_interval_cancels_same_batch_sibling() {
    // PR #78(c), interval flavour: cancelling an interval from an earlier
    // same-deadline timer must suppress both the pending firing and the
    // reschedule `take_due` already pushed back into the queue. (Single
    // manual poll: pre-fix this fires `tick` and reschedules forever, which
    // would hang the deadline-to-deadline drain helper.)
    let mut doc = doc_with_script(
        r#"
        let id2;
        setTimeout(() => { clearInterval(id2); __strake_send_message("t1"); }, 10);
        id2 = setInterval(() => __strake_send_message("tick"), 10);
        "#,
    );
    doc.execute_scripts();
    let deadline = doc.next_timer_deadline().expect("timers scheduled");
    doc.advance_clock_to(deadline);
    doc.poll(None);
    assert_eq!(doc.take_messages(), vec!["t1"]);
    assert!(doc.take_js_errors().is_empty());
    assert_eq!(doc.next_timer_deadline(), None);
}

#[test]
fn interval_reschedule_anchors_to_poll_time() {
    // PR #78(e): interval rescheduling is anchored to poll time (`now +
    // interval`), not to the missed deadline. A late `poll` therefore shifts
    // the interval phase instead of firing catch-up ticks: after polling
    // 25ms past a 10ms interval's deadline, the next deadline is ~10ms (one
    // full interval) after the poll, not immediate.
    use web_time::Duration;

    let mut doc = doc_with_script(
        r#"
        setInterval(() => __strake_send_message("tick"), 10);
        "#,
    );
    doc.execute_scripts();
    let first = doc.next_timer_deadline().expect("interval scheduled");
    // Poll 25ms late for the first tick.
    doc.advance_clock_to(first + Duration::from_millis(25));
    doc.poll(None);
    assert_eq!(doc.take_messages(), vec!["tick"]);
    let next = doc.next_timer_deadline().expect("interval rescheduled");
    assert_eq!(
        next.saturating_duration_since(doc.clock_now()),
        Duration::from_millis(10),
        "next tick is one full interval after the (late) poll, not anchored to the missed deadline"
    );
}

#[test]
fn nested_zero_delay_timer_waits_for_next_poll() {
    // PR #78(e), snapshot semantics: `take_due` snapshots the due batch up
    // front, so a `setTimeout(0)` nested inside `t1` fires on a later `poll`
    // than the already-queued same-deadline sibling `t2` — task-queue order
    // (`t1`, `t2`, then the nested timeout) is preserved across polls.
    let mut doc = doc_with_script(
        r#"
        setTimeout(() => {
            __strake_send_message("t1");
            setTimeout(() => __strake_send_message("nested"), 0);
        }, 0);
        setTimeout(() => __strake_send_message("t2"), 0);
        "#,
    );
    doc.execute_scripts();
    let deadline = doc.next_timer_deadline().expect("timers scheduled");
    doc.advance_clock_to(deadline);
    doc.poll(None);
    assert_eq!(doc.take_messages(), vec!["t1", "t2"]);
    // The nested timeout is pending with (virtual) deadline == now and fires
    // on the next poll.
    assert!(doc.next_timer_deadline().is_some());
    doc.poll(None);
    assert_eq!(doc.take_messages(), vec!["nested"]);
}

#[test]
fn nested_timeout_past_five_levels_clamps_to_4ms() {
    // HTML `timeout()` steps (issue #6 acceptance): a timeout scheduled from
    // a timer task nested more than 5 levels deep clamps to a 4ms minimum.
    // An 8-deep chain of zero-delay timeouts therefore advances virtual time
    // by 0 for its first hops and by >= 4ms once the firing timer's nesting
    // passes 5. Fully deterministic under the virtual clock.
    use web_time::Duration;

    let mut doc = doc_with_script(
        r#"
        function nest(depth) {
            if (depth === 0) { __strake_send_message("done"); return; }
            setTimeout(() => nest(depth - 1), 0);
        }
        nest(8);
        "#,
    );
    doc.execute_scripts();
    let mut prev = None;
    let mut gaps = Vec::new();
    while let Some(deadline) = doc.next_timer_deadline() {
        if let Some(prev) = prev {
            gaps.push(deadline.duration_since(prev));
        }
        prev = Some(deadline);
        doc.advance_clock_to(deadline);
        doc.poll(None);
    }
    assert_eq!(doc.take_messages(), vec!["done"]);
    // 8 chained timeouts, 7 hops: hops scheduled while firing at nesting 0-4
    // stay at 0ms; hops scheduled at firing nesting 5 and 6 (timer nesting 6
    // and 7) clamp to 4ms.
    assert_eq!(gaps.len(), 7, "one hop per chained timeout: {gaps:?}");
    for (i, gap) in gaps.iter().enumerate() {
        if i >= 5 {
            assert!(
                *gap >= Duration::from_millis(4),
                "hop {i} clamps to >= 4ms: {gap:?}"
            );
        } else {
            assert_eq!(*gap, Duration::ZERO, "hop {i} stays unclamped");
        }
    }
}

#[test]
fn engine_trait_drives_backend_through_dyn_dispatch() {
    // The ScriptEngine seam (issue #4, Phase 0): the document's engine must
    // be drivable as a trait object, so a future QuickJS-ng backend can
    // substitute without touching embedders.
    use strake_vibey_script::ScriptEngine;
    let mut doc = doc_with_script("");
    doc.execute_scripts();
    {
        let engine: &mut dyn ScriptEngine = doc.engine_mut();
        engine.eval(r#"__strake_send_message("via-trait");"#, "probe");
    }
    assert_eq!(doc.take_messages(), vec!["via-trait"]);
}
