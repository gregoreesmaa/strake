//! Restyle-invalidation isolation pins (issue #2, section 3D): paint-only
//! style mutations must not trigger Taffy layout passes.

use std::sync::Arc;
use strake_dom::DocumentConfig;
use strake_html::{HtmlDocument, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};

#[test]
fn paint_only_mutation_skips_taffy_layout() {
    let mut doc = HtmlDocument::from_html(
        r#"<!DOCTYPE html><html><head><style>
            body { margin: 0; }
            #box { width: 100px; height: 50px; color: rgb(0, 0, 0); }
        </style></head><body><div id="box">hi</div></body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            font_ctx: Some(strake_dom::hermetic_test_font_context()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let baseline = doc.layout_pass_count();
    assert!(baseline >= 1);

    // Paint-only mutation: restyle must apply with zero Taffy passes.
    let node = doc.query_selector("#box").unwrap().unwrap();
    doc.set_style_property(node, "color", "rgb(0, 0, 255)");
    doc.resolve(0.0);
    assert_eq!(doc.layout_pass_count(), baseline);

    // Layout-affecting mutation: exactly one Taffy pass, geometry updated.
    doc.set_style_property(node, "width", "200px");
    doc.resolve(0.0);
    assert_eq!(doc.layout_pass_count(), baseline + 1);
    let layout = doc.get_node(node).unwrap().final_layout();
    assert_eq!(layout.size.width, 200.0);
}
