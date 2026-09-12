//! Subresource-fetch contract pins.
//!
//! Async fetching (`NetProvider::fetch` + `NetHandler::bytes`) underlies
//! issues #6 (fetch/storage/sockets/timers polyfills), #22 (enterprise
//! network stack), and every render-blocking stylesheet/image/font load.
//! These tests pin the request/response contract that upcoming network
//! work must not regress: URL joining, exactly-once delivery, and
//! no-refetch stability across resolves.

use std::sync::{Arc, Mutex};
use strake_dom::DocumentConfig;
use strake_html::HtmlDocument;
use strake_traits::net::{Bytes, NetHandler, NetProvider, Request};

/// A `NetProvider` which records requests so the test can deliver
/// responses at a time of its choosing (mirrors the proven pattern in
/// `render_blocking_stylesheet.rs`).
#[derive(Default)]
struct ManualNetProvider {
    requests: Mutex<Vec<(String, Box<dyn NetHandler>)>>,
}

impl NetProvider for ManualNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        self.requests
            .lock()
            .unwrap()
            .push((request.url.to_string(), handler));
    }
}

fn requested_urls(net: &ManualNetProvider) -> Vec<String> {
    net.requests
        .lock()
        .unwrap()
        .iter()
        .map(|(url, _)| url.clone())
        .collect()
}

fn make_doc(net: &Arc<ManualNetProvider>, html: &str) -> HtmlDocument {
    HtmlDocument::from_html(
        html,
        DocumentConfig {
            base_url: Some("http://example.com/site/".to_string()),
            net_provider: Some(Arc::clone(net) as _),
            ..Default::default()
        },
    )
}

#[test]
fn relative_stylesheet_urls_resolve_against_base_url() {
    let net = Arc::new(ManualNetProvider::default());
    let _doc = make_doc(
        &net,
        r#"<!doctype html><html><head>
            <link rel="stylesheet" href="css/a.css">
            <link rel="stylesheet" href="/abs/b.css">
        </head><body></body></html>"#,
    );
    let mut urls = requested_urls(&net);
    urls.sort();
    assert_eq!(
        urls,
        vec![
            "http://example.com/abs/b.css".to_string(),
            "http://example.com/site/css/a.css".to_string(),
        ]
    );
}

#[test]
fn each_subresource_is_requested_exactly_once_across_resolves() {
    let net = Arc::new(ManualNetProvider::default());
    let mut doc = make_doc(
        &net,
        r#"<!doctype html><html><head>
            <link rel="stylesheet" href="one.css">
            <link rel="stylesheet" href="two.css">
        </head><body></body></html>"#,
    );
    assert_eq!(requested_urls(&net).len(), 2);
    // Further resolves must not re-issue in-flight requests (no fetch loop).
    doc.resolve(0.0);
    doc.resolve(1.0);
    assert_eq!(requested_urls(&net).len(), 2);
}

#[test]
fn delivered_stylesheet_applies_and_clears_pending_state() {
    let net = Arc::new(ManualNetProvider::default());
    let mut doc = make_doc(
        &net,
        r#"<!doctype html><html><head>
            <link rel="stylesheet" href="theme.css">
        </head><body><div id="box">x</div></body></html>"#,
    );
    assert!(doc.has_pending_critical_resources());
    let (url, handler) = net.requests.lock().unwrap().pop().expect("css requested");
    assert!(url.ends_with("theme.css"));
    handler.bytes(url, Bytes::from_static(b"#box { width: 123px; }"));
    doc.resolve(0.0);
    assert!(!doc.has_pending_critical_resources());
    let node_id = doc.query_selector("#box").unwrap().unwrap();
    let layout = doc.get_node(node_id).unwrap().final_layout();
    assert_eq!(layout.size.width, 123.0);
}
