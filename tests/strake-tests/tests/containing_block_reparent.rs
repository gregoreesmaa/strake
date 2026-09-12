//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/67>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/764>):
//! `position: absolute`/`fixed` boxes must anchor to their containing block
//! (nearest positioned ancestor / viewport), not to their DOM parent's flow
//! position. Taffy resolves an out-of-flow child against its taffy-tree
//! parent, so the layout tree must reparent such boxes at construction.
//! Pure box geometry; no text measurement, so font-independent.

use strake_test_harness::Harness;

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use std::sync::Arc;
use strake_dom::DocumentConfig;
use strake_html::{HtmlDocument, HtmlProvider};
use strake_paint::paint_scene;
use strake_traits::shell::{ColorScheme, Viewport};

fn center_pixel(html: &str) -> [u8; 3] {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(100, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, 100, 100, 0, 0),
        100,
        100,
    );
    let idx = (50 * 100 + 50) * 4;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

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

#[test]
fn grafts_sharing_a_containing_block_keep_document_order() {
    // PR #76 review (a): two abspos boxes sharing `#cb` but living under
    // different static intermediates must graft in document order — same-level
    // paint order is a stable sort over graft order.
    let harness = Harness::from_html(
        r#"<html><head><style>
            #cb { position: relative; width: 200px; height: 200px; }
            .abs { position: absolute; top: 0; left: 0; width: 100px; height: 100px; }
        </style></head><body style="margin:0">
            <div id="cb"><div><div id="a" class="abs"></div></div><div><div id="b" class="abs"></div></div></div>
        </body></html>"#,
    );
    let doc = harness.base();
    let cb = doc.get_node(harness.node("#cb")).expect("cb exists");
    let children = cb.layout_children.borrow();
    let children = children.as_ref().expect("cb has layout children");
    let pos_a = children.iter().position(|id| *id == harness.node("#a"));
    let pos_b = children.iter().position(|id| *id == harness.node("#b"));
    assert!(
        pos_a.is_some() && pos_b.is_some(),
        "both abspos boxes must be grafted onto #cb, got {children:?}"
    );
    assert!(
        pos_a < pos_b,
        "#a must graft before #b (document order), got {children:?}"
    );
}

#[test]
fn later_abspos_sibling_under_another_intermediate_paints_above() {
    // End-to-end paint consequence of the ordering pin above: overlapping
    // auto-z-index abspos siblings `#a` (blue, first) and `#b` (red, later)
    // under different static intermediates; later in document order paints
    // above.
    let px = center_pixel(
        r#"<html><head><style>
            #cb { position: relative; width: 100px; height: 100px; }
            .abs { position: absolute; inset: 0; }
        </style></head><body style="margin:0">
            <div id="cb"><div><div id="a" class="abs" style="background:#0000ff;"></div></div><div><div id="b" class="abs" style="background:#ff0000;"></div></div></div>
        </body></html>"#,
    );
    assert_eq!(
        px,
        [255, 0, 0],
        "later abspos sibling #b must paint above earlier #a"
    );
}

#[test]
fn absolute_under_clipping_intermediate_is_not_reparented() {
    // PR #76 review (b), blast-radius containment: paint follows layout
    // ancestry (`draw_children`/`render_element` accumulate clip_rect and
    // scroll offsets through layout-tree ancestors only), so grafting `#abs`
    // onto `#cb` would let it escape `#clip`'s `overflow: hidden`. Until
    // DOM-ancestor clips are carried through paint, such boxes stay under
    // their DOM parent (pre-#67 anchoring) instead of escaping the clip.
    let harness = Harness::from_html(
        r#"<html><head><style>
            #cb { position: relative; width: 300px; height: 200px; }
            #clip { overflow: hidden; width: 100px; height: 100px; }
            #abs { position: absolute; left: 150px; top: 0; width: 40px; height: 20px; }
        </style></head><body style="margin:0">
            <div id="cb"><div id="clip"><div id="wrap"><div id="abs"></div></div></div></div>
        </body></html>"#,
    );
    let doc = harness.base();
    let abs = doc.get_node(harness.node("#abs")).expect("abs exists");
    assert_eq!(
        abs.layout_parent.get(),
        Some(harness.node("#wrap")),
        "abspos box under a clipping intermediate must not be grafted onto #cb"
    );
}

#[test]
#[ignore = "known MVP limitation (PR #76 review (c)): `fixed` grafts to the root element's box, not the ICB, so root border leaks into insets; want (0,0)"]
fn fixed_ignores_root_element_border() {
    let harness = Harness::from_html(
        r#"<html style="border:5px solid black"><body style="margin:0">
            <div id="fx" style="position: fixed; top: 0; left: 0; width: 26px; height: 14px;"></div>
        </body></html>"#,
    );
    let fx = harness.layout_rect("#fx");
    assert_eq!((fx.x, fx.y), (0.0, 0.0));
}
