//! DOM-mutation hook pins (issue #56, upstream blitz#495).
//!
//! The embedder substrate for the DOM standard: synchronous insert/remove
//! extension hooks plus queued tree-mutation records, fired from the
//! `DocumentMutator` algorithms. A move surfaces compositionally as a
//! remove followed by an insert.

use std::sync::{Arc, Mutex};

use strake_dom::{
    DocumentConfig, LocalName, MutationHooks, MutationRecord, Namespace, NodeId, Prefix, QualName,
    ns,
};
use strake_html::{HtmlDocument, HtmlProvider};

fn qname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(html),
        local: LocalName::from(local),
    }
}

/// Attribute names as `setAttribute` builds them: no namespace.
fn aname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(),
        local: LocalName::from(local),
    }
}

const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

fn xlink_name(local: &str) -> QualName {
    QualName {
        prefix: Some(Prefix::from("xlink")),
        ns: Namespace::from(XLINK_NS),
        local: LocalName::from(local),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HookEvent {
    Inserted { parent: NodeId, child: NodeId },
    Removed { parent: NodeId, child: NodeId },
}

#[derive(Debug, Default)]
struct RecordingHooks {
    events: Mutex<Vec<HookEvent>>,
    records: Mutex<Vec<MutationRecord>>,
}

impl RecordingHooks {
    fn events(&self) -> Vec<HookEvent> {
        self.events.lock().expect("hooks mutex").clone()
    }

    fn records(&self) -> Vec<MutationRecord> {
        self.records.lock().expect("hooks mutex").clone()
    }
}

impl MutationHooks for RecordingHooks {
    fn node_inserted(&self, parent: NodeId, child: NodeId) {
        self.events
            .lock()
            .expect("hooks mutex")
            .push(HookEvent::Inserted { parent, child });
    }

    fn node_removed(&self, parent: NodeId, child: NodeId) {
        self.events
            .lock()
            .expect("hooks mutex")
            .push(HookEvent::Removed { parent, child });
    }

    fn queue_mutation_record(&self, record: MutationRecord) {
        self.records.lock().expect("hooks mutex").push(record);
    }
}

fn make_doc(hooks: Arc<RecordingHooks>) -> HtmlDocument {
    let mut doc = HtmlDocument::from_html(
        r#"<!DOCTYPE html><html><head></head><body><div id="root"></div><div id="other"></div></body></html>"#,
        DocumentConfig {
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        },
    );
    doc.set_mutation_hooks(hooks);
    doc.resolve(0.0);
    doc
}

#[test]
fn insert_attribute_text_and_move_fire_hooks_in_order() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut doc = make_doc(Arc::clone(&hooks));

    let root = doc.query_selector("#root").unwrap().unwrap();
    let other = doc.query_selector("#other").unwrap().unwrap();

    let mut mutator = doc.mutate();
    let para = mutator.create_element(qname("p"), Vec::new());
    let text = mutator.create_text_node("Title");
    mutator.append_children(para, &[text]);
    mutator.set_attribute(para, aname("class"), "note");
    mutator.append_children(root, &[para]);
    // Move root -> other: remove side then insert side.
    mutator.append_children(other, &[para]);
    mutator.set_node_text(text, "Retitled");
    mutator.clear_attribute(para, aname("class"));
    mutator.remove_node(para);
    drop(mutator);

    assert_eq!(
        hooks.events(),
        vec![
            HookEvent::Inserted {
                parent: para,
                child: text
            },
            HookEvent::Inserted {
                parent: root,
                child: para
            },
            HookEvent::Removed {
                parent: root,
                child: para
            },
            HookEvent::Inserted {
                parent: other,
                child: para
            },
            HookEvent::Removed {
                parent: other,
                child: para
            },
        ]
    );
    assert_eq!(
        hooks.records(),
        vec![
            MutationRecord::ChildList {
                target: para,
                added: vec![text],
                removed: Vec::new(),
            },
            MutationRecord::Attributes {
                target: para,
                name: String::from("class"),
                namespace: None,
            },
            MutationRecord::ChildList {
                target: root,
                added: vec![para],
                removed: Vec::new(),
            },
            MutationRecord::ChildList {
                target: root,
                added: Vec::new(),
                removed: vec![para],
            },
            MutationRecord::ChildList {
                target: other,
                added: vec![para],
                removed: Vec::new(),
            },
            MutationRecord::CharacterData { target: text },
            MutationRecord::Attributes {
                target: para,
                name: String::from("class"),
                namespace: None,
            },
            MutationRecord::ChildList {
                target: other,
                added: Vec::new(),
                removed: vec![para],
            },
        ]
    );
}

#[test]
fn drop_all_children_reports_every_child() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut doc = make_doc(Arc::clone(&hooks));

    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut mutator = doc.mutate();
    let first = mutator.create_element(qname("span"), Vec::new());
    let second = mutator.create_element(qname("span"), Vec::new());
    mutator.append_children(root, &[first, second]);
    mutator.remove_and_drop_all_children(root);
    drop(mutator);

    let removed: Vec<NodeId> = hooks
        .events()
        .into_iter()
        .filter_map(|event| match event {
            HookEvent::Removed { child, .. } => Some(child),
            HookEvent::Inserted { .. } => None,
        })
        .collect();
    assert_eq!(removed, vec![first, second]);
    assert!(
        hooks.records().iter().any(|record| matches!(
            record,
            MutationRecord::ChildList { target, removed, .. }
            if *target == root && removed.len() == 2
        )),
        "one batched removal record, got {:?}",
        hooks.records()
    );
}

#[test]
fn mutations_without_hooks_installed_stay_silent() {
    // The default NoopMutationHooks must keep every existing mutation path
    // working with zero observer traffic.
    let mut doc = HtmlDocument::from_html(
        r#"<!DOCTYPE html><html><head></head><body><div id="root"></div></body></html>"#,
        DocumentConfig {
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        },
    );
    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut mutator = doc.mutate();
    let para = mutator.create_element(qname("p"), Vec::new());
    mutator.set_attribute(para, aname("class"), "note");
    mutator.append_children(root, &[para]);
    mutator.remove_node(para);
    drop(mutator);
    doc.resolve(0.0);
}

#[test]
fn style_property_edits_queue_style_attribute_records() {
    // `CSSStyleDeclaration` edits must surface as `style` attribute
    // mutations; no-op edits (unknown property, absent declaration) queue
    // nothing.
    let hooks = Arc::new(RecordingHooks::default());
    let mut doc = make_doc(Arc::clone(&hooks));

    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut mutator = doc.mutate();
    mutator.set_style_property(root, "color", "red");
    mutator.set_style_property(root, "bogus-property", "red");
    mutator.remove_style_property(root, "color");
    mutator.remove_style_property(root, "color");
    drop(mutator);

    assert_eq!(
        hooks.records(),
        vec![
            MutationRecord::Attributes {
                target: root,
                name: String::from("style"),
                namespace: None,
            },
            MutationRecord::Attributes {
                target: root,
                name: String::from("style"),
                namespace: None,
            },
        ]
    );
}

#[test]
fn namespaced_attributes_carry_their_namespace() {
    // `xlink:href` and plain `href` share a local name; the record's
    // namespace must keep them distinguishable on both the set and the
    // clear path. Clearing an absent attribute queues nothing.
    let hooks = Arc::new(RecordingHooks::default());
    let mut doc = make_doc(Arc::clone(&hooks));

    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut mutator = doc.mutate();
    let el = mutator.create_element(qname("div"), Vec::new());
    mutator.append_children(root, &[el]);
    let baseline = hooks.records().len();
    mutator.set_attribute(el, xlink_name("href"), "https://example.com/x");
    mutator.set_attribute(el, aname("href"), "https://example.com/plain");
    mutator.clear_attribute(el, xlink_name("href"));
    mutator.clear_attribute(el, aname("title"));
    drop(mutator);

    assert_eq!(
        hooks.records()[baseline..].to_vec(),
        vec![
            MutationRecord::Attributes {
                target: el,
                name: String::from("href"),
                namespace: Some(String::from(XLINK_NS)),
            },
            MutationRecord::Attributes {
                target: el,
                name: String::from("href"),
                namespace: None,
            },
            MutationRecord::Attributes {
                target: el,
                name: String::from("href"),
                namespace: Some(String::from(XLINK_NS)),
            },
        ]
    );
}

#[test]
fn replace_children_batches_removals_into_one_record() {
    // `replace_children` must fold its bulk removal into a single
    // `ChildList` record (the spec's "replace all" queues one record),
    // matching `remove_and_drop_all_children`. Per-child `node_removed`
    // extension hooks still fire.
    let hooks = Arc::new(RecordingHooks::default());
    let mut doc = make_doc(Arc::clone(&hooks));

    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut mutator = doc.mutate();
    let first = mutator.create_element(qname("span"), Vec::new());
    let second = mutator.create_element(qname("span"), Vec::new());
    let third = mutator.create_element(qname("span"), Vec::new());
    mutator.append_children(root, &[first, second, third]);
    let base_events = hooks.events().len();
    let base_records = hooks.records().len();

    let replacement = mutator.create_element(qname("span"), Vec::new());
    mutator.replace_children(root, &[replacement]);
    drop(mutator);

    assert_eq!(
        hooks.events()[base_events..].to_vec(),
        vec![
            HookEvent::Removed {
                parent: root,
                child: first
            },
            HookEvent::Removed {
                parent: root,
                child: second
            },
            HookEvent::Removed {
                parent: root,
                child: third
            },
            HookEvent::Inserted {
                parent: root,
                child: replacement
            },
        ]
    );
    assert_eq!(
        hooks.records()[base_records..].to_vec(),
        vec![
            MutationRecord::ChildList {
                target: root,
                added: Vec::new(),
                removed: vec![first, second, third],
            },
            MutationRecord::ChildList {
                target: root,
                added: vec![replacement],
                removed: Vec::new(),
            },
        ]
    );
}
