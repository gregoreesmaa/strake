//! Regression pin for <https://github.com/gregoreesmaa/strake/issues/71>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/840>):
//! a CSS `transition` on `transform` must move the painted box, not just
//! the computed style. The painted side reads the cached
//! `Node::transform()`, which has to refresh every animation frame.

use markup5ever::{QualName, local_name, ns};
use strake_dom::DocumentConfig;
use strake_html::HtmlDocument;

const TOGGLE_HTML: &str = r#"<html><head><style>
    .box { position: relative; width: 42px; height: 22px; }
    .dot { position: absolute; top: 0; left: 2px; width: 18px; height: 18px;
           transition: transform 150ms ease; }
</style></head><body style="margin:0">
    <span class="box"><span class="dot" id="dot" style="transform: translateX(0px)"></span></span>
</body></html>"#;

/// Painted translateX of the dot (CSS px), if a transform is cached.
fn painted_translate_x(doc: &HtmlDocument, id: &str) -> Option<f64> {
    let node_id = doc.get_element_by_id(id)?;
    let node = doc.get_node(node_id)?;
    node.transform()
        .as_deref()
        .map(|matrix| matrix.as_coeffs()[4])
}

#[test]
fn transitioned_transform_refreshes_painted_cache() {
    let mut doc = HtmlDocument::from_html(TOGGLE_HTML, DocumentConfig::default());
    doc.resolve(0.0);
    let dot = doc.get_element_by_id("dot").expect("dot exists");

    // Kick the transition like the toggle does at runtime. Note: parsed HTML
    // attributes live in the null namespace (`ns!()`), so the mutation must
    // use it too — `ns!(html)` would append a shadow duplicate that local-name
    // lookups (`attr()`, inline-style flush) never read.
    doc.mutate().set_attribute(
        dot,
        QualName::new(None, ns!(), local_name!("style")),
        "transform: translateX(20px)",
    );

    // Kick frame right after the mutation, the way a live runtime pumps a frame
    // before the next timestamp: Stylo starts the transition at the first time
    // it observes the change, so the mid-flight sample needs a later resolve.
    doc.resolve(0.01);

    // Mid-transition the painted cache must track the computed value.
    doc.resolve(0.1);
    let mid = painted_translate_x(&doc, "dot");
    assert!(
        mid.is_some_and(|tx| tx > 1.0 && tx < 20.0),
        "mid-transition paint follows the animation, got {mid:?}"
    );

    doc.resolve(1.0);
    let end = painted_translate_x(&doc, "dot");
    assert!(
        end.is_some_and(|tx| (tx - 20.0).abs() < 0.5),
        "finished transition paints at 20px, got {end:?}"
    );
}
