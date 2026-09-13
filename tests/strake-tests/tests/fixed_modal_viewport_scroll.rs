//! Regression pin for <https://github.com/gregoreesmaa/strake/issues/72>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/841>):
//! a fixed, translate-centered modal must not make the viewport scrollable.
//! Per css-overflow-3 §3.3 the scrollable overflow area accounts for
//! transforms, so content pulled back inside the window by its transform
//! grants no scroll range. (Offsets are explicit px here: percentage
//! resolution for fixed positioning is a separate gap.)

use strake_dom::DocumentConfig;
use strake_html::HtmlDocument;
use strake_traits::shell::Viewport;

const VIEWPORT: (u32, u32) = (800, 600);

fn doc_with_modal(transform: bool) -> HtmlDocument {
    let config = DocumentConfig {
        viewport: Some(Viewport {
            window_size: VIEWPORT,
            ..Default::default()
        }),
        ..Default::default()
    };
    // Modal border box at (400, 300)..(600, 700): genuinely overflowing the
    // 600px-tall window before the transform, fully inside (300..500,
    // 100..500) once translate(-50%, -50%) applies.
    let html = format!(
        r#"<html><head><style>
        .modal {{ position: fixed; top: 300px; left: 400px; width: 200px; height: 400px;
                 {} background: white; }}
    </style></head><body style="margin:0">
        <div class="modal" id="modal">hi</div>
    </body></html>"#,
        if transform {
            "transform: translate(-50%, -50%);"
        } else {
            ""
        }
    );
    let mut doc = HtmlDocument::from_html(&html, config);
    doc.resolve(0.0);
    doc
}

#[test]
fn translate_centered_fixed_modal_grants_no_viewport_scroll() {
    let mut doc = doc_with_modal(true);
    // Attempt to scroll down past the end of the document.
    doc.scroll_viewport_by(0.0, -10_000.0);
    assert_eq!(doc.viewport_scroll(), strake_dom::Point::ZERO);
}

#[test]
fn untransformed_overflowing_modal_scrolls_by_real_overflow() {
    // Control: without the transform the modal genuinely sticks out
    // (300..700 in a 600px window), so exactly 100px must scroll.
    let mut doc = doc_with_modal(false);
    doc.scroll_viewport_by(0.0, -10_000.0);
    assert_eq!(
        doc.viewport_scroll(),
        strake_dom::Point { x: 0.0, y: 100.0 }
    );
}
