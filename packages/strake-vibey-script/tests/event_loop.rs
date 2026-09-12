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
