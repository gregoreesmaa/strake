//! DOM-mutation hook pins (issue #56, upstream blitz#495).
//!
//! The embedder substrate for the DOM standard: synchronous insert/remove
//! extension hooks plus queued tree-mutation records, fired from the
//! `DocumentMutator` algorithms. A move surfaces compositionally as a
//! remove followed by an insert.

use std::sync::{Arc, Mutex};

use strake_dom::{DocumentConfig, LocalName, MutationHooks, MutationRecord, NodeId, QualName, ns};
use strake_html::{HtmlDocument, HtmlProvider};

fn qname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(html),
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
    mutator.set_attribute(para, qname("class"), "note");
    mutator.append_children(root, &[para]);
    // Move root -> other: remove side then insert side.
    mutator.append_children(other, &[para]);
    mutator.set_node_text(text, "Retitled");
    mutator.clear_attribute(para, qname("class"));
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
    mutator.set_attribute(para, qname("class"), "note");
    mutator.append_children(root, &[para]);
    mutator.remove_node(para);
    drop(mutator);
    doc.resolve(0.0);
}
