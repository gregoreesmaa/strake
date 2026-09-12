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

/// Pins a real shipped provider's failure path instead of a synthetic one:
/// `DataUriNetProvider` serves `data:` URLs synchronously and reports any
/// other scheme via `NetHandler::error` (`UnsupportedScheme`). Before the
/// fix it dropped the handler without any callback, so the document stayed
/// gated behind the render-blocking stylesheet forever.
#[test]
fn real_provider_unsupported_scheme_unblocks_layout() {
    let doc = &mut doc_with(strake_shell::DataUriNetProvider::shared(None));

    // `missing.css` resolves to an `http:` URL, which the data-URI provider
    // rejects through the explicit error path.
    assert!(doc.has_pending_critical_resources());

    doc.resolve(0.0);
    assert!(
        !doc.has_pending_critical_resources(),
        "real provider error must drain the pending-critical set"
    );

    let node_id = doc.get_element_by_id("content").unwrap();
    assert!(
        doc.get_node(node_id).unwrap().primary_styles().is_some(),
        "style resolution must proceed once the real provider reports the failure"
    );
}

/// A provider that drops every fetch without a callback (transport failure,
/// abort, third-party drop) while counting per-URL attempts, so the failure
/// is delivered only through the recipient's drop-backstop.
struct CountingDroppingNetProvider {
    counts: std::sync::Mutex<std::collections::HashMap<String, usize>>,
}

impl CountingDroppingNetProvider {
    fn new() -> Self {
        Self {
            counts: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn count_for(&self, url: &str) -> usize {
        self.counts.lock().unwrap().get(url).copied().unwrap_or(0)
    }
}

impl NetProvider for CountingDroppingNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url.to_string();
        *self.counts.lock().unwrap().entry(url).or_insert(0) += 1;
        drop(handler);
    }
}

const IMG_HTML: &str = r#"<!doctype html>
<html>
  <head></head>
  <body><img id="pic" src="a.png"></body>
</html>"#;

fn src_qname() -> strake_dom::QualName {
    strake_dom::QualName::new(None, strake_dom::ns!(), strake_dom::local_name!("src"))
}

/// A dropped image fetch must drain its `pending_images` entry: re-requesting
/// the same URL after the failure was ingested must issue a new fetch.
/// Before the backstop reported the request URL, the dropped fetch left a
/// stale entry behind and the re-request coalesced onto it, so the image
/// (and any later same-URL mutation) stalled forever with no fetch issued.
#[test]
fn failed_image_fetch_drains_so_rerequest_refetches() {
    let provider = Arc::new(CountingDroppingNetProvider::new());
    let doc = &mut HtmlDocument::from_html(
        IMG_HTML,
        DocumentConfig {
            base_url: Some("http://example.com/".to_string()),
            net_provider: Some(provider.clone()),
            ..Default::default()
        },
    );

    let pic = doc.get_element_by_id("pic").unwrap();
    doc.resolve(0.0);
    assert_eq!(provider.count_for("http://example.com/a.png"), 1);

    // Point the image elsewhere and back; each distinct request fails, but
    // every request must still reach the provider.
    doc.mutate().set_attribute(pic, src_qname(), "b.png");
    doc.resolve(0.0);
    assert_eq!(provider.count_for("http://example.com/b.png"), 1);

    doc.mutate().set_attribute(pic, src_qname(), "a.png");
    doc.resolve(0.0);
    assert_eq!(
        provider.count_for("http://example.com/a.png"),
        2,
        "re-requesting a failed image URL must issue a new fetch instead of queueing onto the dead entry"
    );
}
