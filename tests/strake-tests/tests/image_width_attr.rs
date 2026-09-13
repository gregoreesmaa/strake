//! Verify pin for <https://github.com/gregoreesmaa/strake/issues/49>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/392>):
//! a `width`/`height` *attribute* on `<img>` must reach layout exactly like
//! the same declaration in `style` does — Taffy must not see `auto`.
//!
//! Triage: does not reproduce on current main — attribute and style agree.
//! This locks the parity in place.

use strake_test_harness::Harness;

fn img_box(html: &str, sel: &str) -> (f32, f32) {
    let harness = Harness::from_html(html);
    let id = harness.node(sel);
    let base = harness.base();
    let node = base.get_node(id).unwrap();
    let l = node.final_layout();
    (l.size.width, l.size.height)
}

const BODY: &str = r#"<html><body style="margin:0">
    <img id="a" width="100" height="40" src="about:blank">
    <img id="s" style="width:100px; height:40px;" src="about:blank">
</body></html>"#;

#[test]
fn img_width_attribute_reaches_layout() {
    assert_eq!(img_box(BODY, "#a"), (100.0, 40.0));
}

#[test]
fn img_width_attribute_matches_style() {
    assert_eq!(img_box(BODY, "#a"), img_box(BODY, "#s"));
}
