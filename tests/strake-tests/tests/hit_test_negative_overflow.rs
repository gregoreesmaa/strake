//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/33>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/198>):
//! children overflowing their parent in the negative direction (top/left)
//! must still be hittable where they are exposed outside the parent's box.

use strake_test_harness::{Harness, HarnessOptions};

fn harness(html: &str) -> Harness {
    Harness::from_html_with(
        html,
        HarnessOptions {
            width: 300,
            height: 300,
            ..Default::default()
        },
    )
}

#[test]
fn child_overflowing_parent_top_left_is_hittable() {
    let harness = harness(
        r#"<html><body style="margin:0">
        <div id="parent" style="margin-left:50px; margin-top:50px; width:100px; height:100px; background:blue;">
            <div id="child" style="margin-left:-30px; margin-top:-20px; width:40px; height:40px; background:red;"></div>
        </div>
    </body></html>"#,
    );
    let child = harness.node("#child");
    // (25,35) is inside #child (20,30)-(60,70) but outside #parent (50,50)-(150,150).
    assert_eq!(harness.hit_node(25.0, 35.0), child);
    // The part overlapping the parent still hits the child (paints above).
    assert_eq!(harness.hit_node(55.0, 55.0), child);
}

#[test]
fn abspos_child_overflowing_parent_top_left_is_hittable() {
    let harness = harness(
        r#"<html><body style="margin:0">
        <div id="parent" style="position:relative; margin-left:50px; margin-top:50px; width:100px; height:100px; background:blue;">
            <div id="child" style="position:absolute; left:-30px; top:-20px; width:40px; height:40px; background:red;"></div>
        </div>
    </body></html>"#,
    );
    let child = harness.node("#child");
    assert_eq!(harness.hit_node(25.0, 35.0), child);
}
