//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/65>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/694>):
//! a failed render-blocking fetch must still call back so the document
//! unblocks. Browsers treat a failed render-blocking stylesheet as
//! "loaded with zero rules" and proceed with rendering instead of
//! blocking the page forever.

use std::sync::Arc;
use strake_dom::DocumentConfig;
use strake_html::HtmlDocument;
use strake_traits::net::{NetHandler, NetProvider, Request};

const HTML: &str = r#"<!doctype html>
<html>
  <head><link rel="stylesheet" href="missing.css"></head>
  <body><div id="content">text</div></body>
</html>"#;

fn doc_with(provider: Arc<dyn NetProvider>) -> HtmlDocument {
    HtmlDocument::from_html(
        HTML,
        DocumentConfig {
            base_url: Some("http://example.com/".to_string()),
            net_provider: Some(provider),
            ..Default::default()
        },
    )
}

/// Simulates a transport-level failure the way `strake-net`'s
/// `Provider::fetch` behaved before the fix: the error is logged and the
/// handler is dropped without any callback.
struct DroppingNetProvider;

impl NetProvider for DroppingNetProvider {
    fn fetch(&self, _doc_id: usize, _request: Request, handler: Box<dyn NetHandler>) {
        drop(handler);
    }
}

#[test]
fn dropped_render_blocking_fetch_unblocks_layout() {
    let doc = &mut doc_with(Arc::new(DroppingNetProvider));

    // The stylesheet fetch was issued and is tracked as render-blocking.
    assert!(doc.has_pending_critical_resources());

    // Resolving must ingest the failure and proceed past the
    // critical-resource gate instead of blocking forever.
    doc.resolve(0.0);
    assert!(
        !doc.has_pending_critical_resources(),
        "failed fetch must drain the pending-critical set (browsers treat it as zero rules)"
    );

    // Style resolution must have run past the gate.
    let node_id = doc.get_element_by_id("content").unwrap();
    assert!(
        doc.get_node(node_id).unwrap().primary_styles().is_some(),
        "style resolution must proceed once the failed fetch reports back"
    );
}

/// A provider that reports the failure explicitly via the `NetHandler::error`
/// failure path must unblock the document the same way.
struct ExplicitErrorNetProvider;

impl NetProvider for ExplicitErrorNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        handler.error(request.url.to_string(), String::from("boom"));
    }
}

#[test]
fn explicit_error_callback_unblocks_layout() {
    let doc = &mut doc_with(Arc::new(ExplicitErrorNetProvider));

    assert!(doc.has_pending_critical_resources());

    doc.resolve(0.0);
    assert!(
        !doc.has_pending_critical_resources(),
        "explicit error callback must drain the pending-critical set"
    );

    let node_id = doc.get_element_by_id("content").unwrap();
    assert!(
        doc.get_node(node_id).unwrap().primary_styles().is_some(),
        "style resolution must proceed once the error callback fires"
    );
}
