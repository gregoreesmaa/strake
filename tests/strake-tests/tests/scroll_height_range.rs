//! Verify pin for <https://github.com/gregoreesmaa/strake/issues/50>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/444>):
//! a scroll container's range must span the full bottom of its children —
//! scrolling to the end must reach the bottom-most content, not clamp early.
//!
//! Triage: does not reproduce on current main — the upstream repro scrolls
//! the full 230px. This locks the range in place.

use strake_dom::ScrollBehavior;
use strake_test_harness::Harness;

fn max_scroll_y(html: &str) -> f64 {
    let mut harness = Harness::from_html(html);
    let scroller = harness.node("#scroller");
    harness
        .base_mut()
        .scroll_by(scroller, 0.0, 10_000.0, ScrollBehavior::Instant);
    let base = harness.base();
    let node = base.get_node(scroller).unwrap();
    node.scroll_offset().y
}

// Exact upstream repro: 200px auto container, padding 10px, children
// 150 + 15 + 150 + 15 + 100 = 450px of content in a 220px border box.
const UPSTREAM: &str = r#"<html><body style="margin:0">
    <div id="scroller" style="width:200px; height:200px; overflow-y:auto; border:0; padding:10px;">
        <div style="height:150px; margin-bottom:15px;"></div>
        <div style="height:150px; margin-bottom:15px;"></div>
        <div style="height:100px;"></div>
    </div>
</body></html>"#;

#[test]
fn scroll_range_reaches_bottom_most_child() {
    assert_eq!(max_scroll_y(UPSTREAM), 230.0);
}

#[test]
fn scroll_range_covers_absolute_child() {
    let html = r#"<html><body style="margin:0">
        <div id="scroller" style="width:200px; height:200px; overflow-y:auto; position:relative;">
            <div style="position:absolute; top:0; left:0; width:50px; height:500px;"></div>
        </div>
    </body></html>"#;
    assert_eq!(max_scroll_y(html), 300.0);
}
