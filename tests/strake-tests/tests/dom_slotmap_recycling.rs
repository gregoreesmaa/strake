//! SlotMap generational-stability pins (issue #2, section 3A).

use std::sync::Arc;
use strake_dom::{DocumentConfig, LocalName, QualName, ns};
use strake_html::{HtmlDocument, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};

fn qname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(html),
        local: LocalName::from(local),
    }
}

fn make_doc() -> HtmlDocument {
    let doc = HtmlDocument::from_html(
        r#"<!DOCTYPE html><html><head><style>
            body { margin: 0; }
            #btn[disabled] { color: rgb(255, 0, 0); }
            #btn { color: rgb(0, 0, 0); }
        </style></head><body><div id="root"></div></body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            font_ctx: Some(strake_dom::hermetic_test_font_context()),
            ..Default::default()
        },
    );
    let mut doc = doc;
    doc.resolve(0.0);
    doc
}

fn color_of(doc: &strake_dom::BaseDocument, selector: &str) -> [u8; 3] {
    // Verbatim from tests/strake-tests/tests/style_property_invalidation.rs:45-56.
    let node_id = doc.query_selector(selector).unwrap().unwrap();
    let node = doc.get_node(node_id).unwrap();
    let styles = node.primary_styles().unwrap();
    let color = styles.clone_color().into_srgb_legacy();
    let srgb = color.raw_components();
    [
        (srgb[0] * 255.0).round() as u8,
        (srgb[1] * 255.0).round() as u8,
        (srgb[2] * 255.0).round() as u8,
    ]
}

#[test]
fn stale_node_id_never_aliases_recycled_slot() {
    let mut doc = make_doc();
    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut stale = None;
    for i in 0..10_000 {
        let mut m = doc.mutate();
        let el = m.create_element(qname("div"), Vec::new());
        m.append_children(root, &[el]);
        if i == 0 {
            stale = Some(el);
        }
        m.remove_and_drop_node(el);
    }
    drop(doc.mutate());
    doc.resolve(0.0);
    // The very first id must be dead even after 10k slot reuses.
    assert!(doc.get_node(stale.unwrap()).is_none());
    // And fresh allocations still work.
    let mut m = doc.mutate();
    let el = m.create_element(qname("div"), Vec::new());
    m.append_children(root, &[el]);
    drop(m);
    doc.resolve(0.0);
    assert!(doc.get_node(el).is_some());
}

#[test]
fn detached_subtree_reparent_keeps_document_order() {
    let mut doc = make_doc();
    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut m = doc.mutate();
    let parent = m.create_element(qname("div"), Vec::new());
    let child = m.create_element(qname("span"), Vec::new());
    let text = m.create_text_node("hi");
    m.append_children(child, &[text]);
    m.append_children(parent, &[child]);
    m.append_children(root, &[parent]);
    // Detach the whole subtree, then re-attach: descendants survive.
    // (Drop the mutator before reading back: it holds `&mut doc`.)
    m.remove_node(parent);
    drop(m);
    assert!(doc.get_node(child).is_some());
    let mut m = doc.mutate();
    m.append_children(root, &[parent]);
    drop(m);
    doc.resolve(0.0);
    assert!(doc.query_selector("span").unwrap().is_some());
    assert_eq!(
        doc.query_selector("#root div span").unwrap().is_some(),
        true
    );
}

#[test]
fn disabled_attribute_reflection_changes_matched_style() {
    let mut doc = make_doc();
    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut m = doc.mutate();
    let btn = m.create_element(qname("button"), Vec::new());
    m.set_attribute(btn, qname("id"), "btn");
    m.append_children(root, &[btn]);
    drop(m);
    doc.resolve(0.0);
    assert_eq!(color_of(&doc, "#btn"), [0, 0, 0]);
    let mut m = doc.mutate();
    m.set_attribute(btn, qname("disabled"), "");
    drop(m);
    doc.resolve(0.0);
    assert_eq!(color_of(&doc, "#btn"), [255, 0, 0]);
    let mut m = doc.mutate();
    m.clear_attribute(btn, qname("disabled"));
    drop(m);
    doc.resolve(0.0);
    assert_eq!(color_of(&doc, "#btn"), [0, 0, 0]);
}
