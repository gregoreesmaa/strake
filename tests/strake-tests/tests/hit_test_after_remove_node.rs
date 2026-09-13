//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/62>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/624>):
//! `remove_and_drop_node` must purge the dropped ids from the derived
//! `paint_children` and stacking-context hoisted-children lists. Those lists
//! rebuild on the next `resolve()`, but a hit test running in between must
//! not panic on stale ids — it should land on whatever is underneath.

use strake_test_harness::{Harness, HarnessOptions};
use strake_traits::node_id::NodeId;

/// Ids on a node's stacking-context hoisted list (empty when it has none).
fn hoisted_ids(harness: &Harness, id: NodeId) -> Vec<NodeId> {
    harness
        .base()
        .get_node(id)
        .and_then(|node| node.stacking_context.as_ref())
        .map(|sc| sc.children.iter().map(|child| child.node_id).collect())
        .unwrap_or_default()
}

fn harness(html: &str) -> Harness {
    Harness::from_html_with(
        html,
        HarnessOptions {
            width: 200,
            height: 200,
            ..Default::default()
        },
    )
}

const REMOVABLE_HTML: &str = r#"<html><body style="margin:0">
    <div id="parent" style="position:relative; width:200px; height:200px; background:blue;">
        <div id="victim" style="position:absolute; left:10px; top:10px; width:100px; height:100px; background:red;"></div>
        <div id="sibling" style="position:absolute; left:120px; top:10px; width:60px; height:60px; background:green;"></div>
    </div>
</body></html>"#;

#[test]
fn hit_after_remove_node_does_not_panic() {
    let mut harness = harness(REMOVABLE_HTML);
    let victim = harness.node("#victim");
    let parent = harness.node("#parent");

    // Sanity: the victim is hittable before removal.
    assert_eq!(harness.hit_node(20.0, 20.0), victim);

    // Remove without resolving: derived paint lists still name the victim.
    harness.base_mut().mutate().remove_and_drop_node(victim);

    // Must not panic: the hit lands on the parent underneath.
    assert_eq!(harness.hit_node(20.0, 20.0), parent);
    // The untouched sibling still hits.
    assert_eq!(harness.hit_node(130.0, 20.0), harness.node("#sibling"));
}

#[test]
fn hit_after_remove_hoisted_node_does_not_panic() {
    let mut harness = harness(
        r#"<html><body style="margin:0">
        <div id="parent" style="position:relative; z-index:0; width:200px; height:200px; background:blue;">
            <div id="victim" style="position:absolute; z-index:5; left:10px; top:10px; width:100px; height:100px; background:red;"></div>
        </div>
    </body></html>"#,
    );
    let victim = harness.node("#victim");
    let parent = harness.node("#parent");

    // Setup: the parent stacking context hoists the z-index victim.
    let hoisted_before = hoisted_ids(&harness, parent);
    assert!(
        hoisted_before.contains(&victim),
        "victim must be hoisted pre-removal, got {hoisted_before:?}"
    );

    harness.base_mut().mutate().remove_and_drop_node(victim);

    // The derived entry is purged (full rebuild happens on next resolve).
    let hoisted_after = hoisted_ids(&harness, parent);
    assert!(
        !hoisted_after.contains(&victim),
        "stale hoisted entry purged, got {hoisted_after:?}"
    );

    // And hitting the old area does not panic.
    let _ = harness.hit(20.0, 20.0);
}

#[test]
fn hit_after_remove_all_children_does_not_panic() {
    let mut harness = harness(REMOVABLE_HTML);
    let parent = harness.node("#parent");

    harness
        .base_mut()
        .mutate()
        .remove_and_drop_all_children(parent);

    // Must not panic: the emptied parent itself is hit.
    assert_eq!(harness.hit_node(20.0, 20.0), parent);
}
