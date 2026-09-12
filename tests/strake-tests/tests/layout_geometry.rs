//! Flexbox/Grid geometry pins (issue #2, section 3B). Pure box geometry;
//! no text measurement, so results are font-independent.
//!
//! NOTE: subgrid and dense auto-placement are out of scope (Taffy does not
//! implement subgrid).

use strake_test_harness::Harness;

#[test]
fn flex_auto_margins_absorb_free_space() {
    let harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <div id="row" style="display:flex; width:300px; height:50px;">
                <div id="a" style="width:50px; height:50px; margin-left:auto;"></div>
            </div>
        </body></html>"#,
    );
    let row = harness.layout_rect("#row");
    let a = harness.layout_rect("#a");
    assert_eq!((row.x, row.y, row.width), (0.0, 0.0, 300.0));
    assert_eq!((a.x, a.width), (250.0, 50.0));
}

#[test]
fn flex_wrap_creates_two_rows() {
    let harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <div id="row" style="display:flex; flex-wrap:wrap; width:120px;">
                <div id="a" style="width:100px; height:10px;"></div>
                <div id="b" style="width:100px; height:10px;"></div>
            </div>
        </body></html>"#,
    );
    let a = harness.layout_rect("#a");
    let b = harness.layout_rect("#b");
    assert_eq!((a.x, a.y), (0.0, 0.0));
    assert_eq!((b.x, b.y), (0.0, 10.0));
}

#[test]
fn grid_fr_tracks_split_free_space_with_gap() {
    let harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <div id="g" style="display:grid; grid-template-columns:1fr 2fr; column-gap:10px; width:310px; height:40px;">
                <div id="a"></div><div id="b"></div>
            </div>
        </body></html>"#,
    );
    // Free space 310-10=300 split 1:2 -> 100 / 200.
    let a = harness.layout_rect("#a");
    let b = harness.layout_rect("#b");
    assert_eq!((a.x, a.width), (0.0, 100.0));
    assert_eq!((b.x, b.width), (110.0, 200.0));
}
