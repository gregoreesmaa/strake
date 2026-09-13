//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/55>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/490>):
//! relative resource URLs with no usable base URL (the default `data:`
//! base) must not panic the resolver. Unresolvable stylesheets and images
//! skip their fetch; the page still parses and lays out.

use strake_dom::DocumentConfig;
use strake_html::HtmlDocument;

#[test]
fn relative_stylesheet_without_base_url_does_not_panic() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><head><link rel="stylesheet" href="./styles.css"></head>
        <body><p>hi</p></body></html>"#,
        DocumentConfig::default(),
    );
    doc.resolve(0.0);
    let text = doc.find_body_node().expect("body parsed").text_content();
    assert!(
        text.contains("hi"),
        "page parses and lays out despite the unloadable sheet: {text:?}"
    );
}

#[test]
fn relative_image_without_base_url_does_not_panic() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0"><img src="./x.png" width="10" height="10"></body></html>"#,
        DocumentConfig::default(),
    );
    doc.resolve(0.0);
}
