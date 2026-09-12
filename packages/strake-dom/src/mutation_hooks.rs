//! Synchronous DOM-mutation hooks for embedders (issue #56, upstream
//! blitz#495).
//!
//! An embedder implementing the DOM standard needs to observe tree mutations
//! from inside the mutation algorithms: queueing tree-mutation records (for
//! `MutationObserver` compound microtasks), slot-change signaling, and the
//! `insert` / `post-connection` / `remove` / `move` extension points. The
//! [`DocumentMutator`](crate::DocumentMutator) calls into
//! [`MutationHooks`] synchronously from exactly those places, mirroring how
//! event handling goes through the [`EventHandler`](crate::EventHandler)
//! trait.
//!
//! Registration lives on the document, next to `shell_provider`:
//! [`BaseDocument::set_mutation_hooks`](crate::BaseDocument::set_mutation_hooks)
//! (default [`NoopMutationHooks`]). Hooks fire for every mutation, connected
//! or not; the embedder filters by its observed targets. A move surfaces
//! compositionally as `node_removed` (old parent) followed by
//! `node_inserted` (new parent). Slot-change signaling is future work: shadow
//! DOM does not exist yet, and per upstream it is only useful once it does.
//!
//! Hooks must not mutate the document: they run mid-algorithm while the
//! mutator holds document borrows, so re-entrant mutation is unsupported.

use strake_traits::node_id::NodeId;

/// One DOM-standard tree mutation record, queued for the embedder's observer
/// machinery (upstream `queue-a-tree-mutation-record`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationRecord {
    /// Children added and/or removed on `target`.
    ChildList {
        /// The parent whose children changed.
        target: NodeId,
        /// Inserted children, in insertion order.
        added: Vec<NodeId>,
        /// Detached children, in detachment order.
        removed: Vec<NodeId>,
    },
    /// An attribute was set or removed on `target`. Edits through a
    /// `CSSStyleDeclaration` surface here as a `style` attribute mutation.
    Attributes {
        /// The element whose attribute changed.
        target: NodeId,
        /// Local attribute name.
        name: String,
        /// Attribute namespace URI (the DOM record's `attributeNamespace`):
        /// `None` for the empty (null) namespace, so a plain `href` reports
        /// `None` while `xlink:href` reports the XLink namespace URI.
        namespace: Option<String>,
    },
    /// Character data changed inside `target` (a text node).
    CharacterData {
        /// The text node whose data changed.
        target: NodeId,
    },
}

/// Synchronous embedder hooks into the DOM mutation algorithms.
///
/// All methods take `&self` (implementations use interior mutability) so the
/// hooks can ride on the document as a shared `Arc`, exactly like
/// [`ShellProvider`](strake_traits::shell::ShellProvider).
pub trait MutationHooks: Send + Sync {
    /// Extension point for "insert a node" (covers the spec's insert and
    /// post-connection steps): `child` is linked under `parent`.
    fn node_inserted(&self, parent: NodeId, child: NodeId);

    /// Extension point for "remove a node" (covers remove and, paired with a
    /// later insert, move): `child` is unlinked from `parent`.
    fn node_removed(&self, parent: NodeId, child: NodeId);

    /// Queue one tree-mutation record for observer dispatch.
    ///
    /// Record granularity is caller-dependent, matching the DOM standard's
    /// per-operation batching: single-node ops (`remove_node`, and each
    /// detach inside `add_children_to_parent`) queue one record per node,
    /// while bulk ops (`remove_and_drop_all_children`, `replace_children`,
    /// and the insert side of `add_children_to_parent`) fold the whole
    /// operation into a single `ChildList` record. Embedders synthesizing
    /// spec `MutationObserver` records must preserve record boundaries
    /// rather than assume one record per node.
    fn queue_mutation_record(&self, record: MutationRecord);
}

/// Default hooks: observe nothing.
pub struct NoopMutationHooks;

impl MutationHooks for NoopMutationHooks {
    fn node_inserted(&self, _parent: NodeId, _child: NodeId) {}
    fn node_removed(&self, _parent: NodeId, _child: NodeId) {}
    fn queue_mutation_record(&self, _record: MutationRecord) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingHooks {
        events: Mutex<Vec<String>>,
        records: Mutex<Vec<MutationRecord>>,
    }

    impl MutationHooks for RecordingHooks {
        fn node_inserted(&self, parent: NodeId, child: NodeId) {
            self.events
                .lock()
                .expect("m")
                .push(format!("inserted {child:?} into {parent:?}"));
        }

        fn node_removed(&self, parent: NodeId, child: NodeId) {
            self.events
                .lock()
                .expect("m")
                .push(format!("removed {child:?} from {parent:?}"));
        }

        fn queue_mutation_record(&self, record: MutationRecord) {
            self.records.lock().expect("m").push(record);
        }
    }

    #[test]
    fn hooks_are_object_safe_and_send_sync() {
        fn assert_bounds(_: &dyn MutationHooks) {}
        let hooks = RecordingHooks {
            events: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
        };
        fn is_send_sync<T: Send + Sync>() {}
        is_send_sync::<RecordingHooks>();
        assert_bounds(&hooks);
        assert_bounds(&NoopMutationHooks);
    }
}
