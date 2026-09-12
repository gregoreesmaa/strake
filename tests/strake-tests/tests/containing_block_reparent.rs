//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/67>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/764>):
//! `position: absolute`/`fixed` boxes must anchor to their containing block
//! (nearest positioned ancestor / viewport), not to their DOM parent's flow
//! position. Taffy resolves an out-of-flow child against its taffy-tree
//! parent, so the layout tree must reparent such boxes at construction.
//! Pure box geometry; no text measurement, so font-independent.

use strake_test_harness::Harness;

#[test]
fn absolute_anchors_to_positioned_ancestor_not_dom_parent() {
    let harness = Harness::from_html(
        r#"<html><head><style>
            #gp { position: relative; width: 300px; height: 200px; padding-top: 1px; }
            #mid { width: 200px; height: 100px; margin-left: 30px; }
            #abs { position: absolute; top: 5px; left: 15px; width: 40px; height: 20px; }
        </style></head><body style="margin:0">
            <div id="gp"><div id="mid"><div id="abs"></div></div></div>
        </body></html>"#,
    );
    // Control: the in-flow intermediate parent is unaffected.
    let mid = harness.layout_rect("#mid");
    assert_eq!((mid.x, mid.y), (30.0, 1.0));
    // `#abs` insets resolve against `#gp`'s padding box, not `#mid`'s border box.
    let abs_rect = harness.layout_rect("#abs");
    assert_eq!((abs_rect.x, abs_rect.y), (15.0, 5.0));
}

#[test]
fn fixed_anchors_to_viewport_not_dom_parent() {
    let harness = Harness::from_html(
        r#"<html><body style="margin:8px">
            <div id="fx" style="position: fixed; top: 0; left: 0; width: 26px; height: 14px;"></div>
        </body></html>"#,
    );
    let fx = harness.layout_rect("#fx");
    assert_eq!((fx.x, fx.y), (0.0, 0.0));
}

#[test]
fn nested_absolute_anchors_to_nearest_positioned_ancestor() {
    let harness = Harness::from_html(
        r#"<html><head><style>
            #outer { position: absolute; top: 20px; left: 30px; width: 200px; height: 150px; }
            #inner { position: absolute; top: 4px; left: 6px; width: 40px; height: 20px; }
        </style></head><body style="margin:0">
            <div id="outer"><div><div id="inner"></div></div></div>
        </body></html>"#,
    );
    // `#outer` has no positioned ancestor: ICB + insets.
    let outer = harness.layout_rect("#outer");
    assert_eq!((outer.x, outer.y), (30.0, 20.0));
    // `#inner` anchors to `#outer` (itself positioned), skipping the static
    // intermediate div.
    let inner = harness.layout_rect("#inner");
    assert_eq!((inner.x, inner.y), (36.0, 24.0));
}

#[test]
fn reparenting_is_stable_across_resolves() {
    let mut harness = Harness::from_html(
        r#"<html><head><style>
            #gp { position: relative; width: 300px; height: 200px; }
            #abs { position: absolute; top: 5px; left: 15px; width: 40px; height: 20px; }
        </style></head><body style="margin:0">
            <div id="gp"><div id="mid"><div id="abs"></div></div></div>
        </body></html>"#,
    );
    let before = harness.layout_rect("#abs");
    assert_eq!((before.x, before.y), (15.0, 5.0));
    // Steady-state frames must not move the box again (no duplicates,
    // no flip-flopping between parents).
    harness.pump();
    harness.pump();
    let after = harness.layout_rect("#abs");
    assert_eq!((after.x, after.y), (15.0, 5.0));
}

#[test]
fn absolute_without_positioned_ancestor_anchors_to_initial_containing_block() {
    let harness = Harness::from_html(
        r#"<html><head><style>
            #mid { width: 200px; height: 100px; margin-top: 40px; }
            #abs { position: absolute; top: 5px; left: 10px; width: 40px; height: 20px; }
        </style></head><body style="margin:0">
            <div id="mid"><div id="abs"></div></div>
        </body></html>"#,
    );
    // No positioned ancestor: insets resolve against the initial containing
    // block (viewport origin), not `#mid`'s displaced flow position.
    let abs_rect = harness.layout_rect("#abs");
    assert_eq!((abs_rect.x, abs_rect.y), (10.0, 5.0));
}
