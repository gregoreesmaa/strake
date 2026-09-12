//! Capture-phase event dispatch pins (issue #2, section 3C).

use keyboard_types::Modifiers;
use strake_dom::{Document, DocumentConfig};
use strake_traits::events::{DomEvent, DomEventData, StrakeFocusEvent};
use strake_vibey_script::ScriptDocument;

fn doc_from_html(html: &str) -> ScriptDocument {
    let mut doc = ScriptDocument::from_html(html, DocumentConfig::default());
    doc.execute_scripts();
    doc
}

fn text_of_selector(doc: &ScriptDocument, selector: &str) -> String {
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}

fn click_on(doc: &ScriptDocument, selector: &str) -> DomEvent {
    let inner = doc.inner();
    let id = inner.query_selector(selector).unwrap().unwrap();
    DomEvent::new(
        id,
        inner
            .get_node(id)
            .unwrap()
            .synthetic_click_event(Modifiers::empty()),
    )
}

fn focus_on(doc: &ScriptDocument, selector: &str) -> DomEvent {
    let inner = doc.inner();
    let id = inner.query_selector(selector).unwrap().unwrap();
    // `focus` does not bubble (`DomEventData::bubbles() == false`).
    DomEvent::new(id, DomEventData::Focus(StrakeFocusEvent))
}

#[test]
fn capture_listeners_fire_root_to_target_before_bubble() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="outer"><div id="middle"><button id="inner">hi</button></div></div>
            <div id="out"></div>
            <script>
                const log = [];
                const record = (name) => () => {
                    log.push(name);
                    document.getElementById("out").textContent = log.join(",");
                };
                for (const id of ["outer", "middle", "inner"]) {
                    document.getElementById(id).addEventListener("click", record("cap:" + id), true);
                    document.getElementById(id).addEventListener("click", record("bub:" + id), false);
                }
            </script>
        </body></html>
        "#,
    );
    doc.dispatch_dom_event(click_on(&doc, "#inner"));
    // Note: at-target capture listeners run before bubble listeners here;
    // the modern DOM fires at-target listeners in registration order
    // regardless of capture. Deviation tracked for issue #10 follow-up.
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "cap:outer,cap:middle,cap:inner,bub:inner,bub:middle,bub:outer"
    );
}

#[test]
fn stop_propagation_in_capture_prevents_bubble_phase() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="outer"><button id="inner">hi</button></div>
            <div id="out"></div>
            <script>
                const log = [];
                const record = (name) => () => {
                    log.push(name);
                    document.getElementById("out").textContent = log.join(",");
                };
                document.getElementById("outer").addEventListener("click", (e) => {
                    record("cap:outer")();
                    e.stopPropagation();
                }, true);
                document.getElementById("inner").addEventListener("click", record("bub:inner"), false);
                document.getElementById("outer").addEventListener("click", record("bub:outer"), false);
            </script>
        </body></html>
        "#,
    );
    doc.dispatch_dom_event(click_on(&doc, "#inner"));
    assert_eq!(text_of_selector(&doc, "#out"), "cap:outer");
}

#[test]
fn at_target_capture_listener_fires_for_non_bubbling_event() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="outer"><button id="inner">hi</button></div>
            <div id="out"></div>
            <script>
                const log = [];
                const record = (name) => () => {
                    log.push(name);
                    document.getElementById("out").textContent = log.join(",");
                };
                document.getElementById("inner").addEventListener("focus", record("cap:inner"), true);
                document.getElementById("inner").addEventListener("focus", record("bub:inner"), false);
                document.getElementById("outer").addEventListener("focus", record("bub:outer"), false);
            </script>
        </body></html>
        "#,
    );
    doc.dispatch_dom_event(focus_on(&doc, "#inner"));
    assert_eq!(text_of_selector(&doc, "#out"), "cap:inner,bub:inner");
}
