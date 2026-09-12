//! Origin-partitioned web storage (`localStorage` / `sessionStorage`).
//!
//! Issue #6's amendment orders the storage work after fetch+timers:
//! `localStorage`/`sessionStorage`/clipboard next (clipboard shipped in
//! #95), `WebSocket`/`EventSource` after that, IndexedDB last. This module
//! is the `localStorage`/`sessionStorage` step with exact Web Storage
//! semantics — string coercion happens at the JS binding, so this layer
//! stores strings — origin partitioning, insertion-ordered `key(n)`, a 5MB
//! quota, and a change feed for the future `storage` event dispatch.
//! Persistence (redb/SQLite backends, cross-window sync) binds the
//! [`StorageBackend`] seam next; the stores themselves are backend-agnostic.

use std::collections::HashMap;

/// Quota per storage area (the conventional 5MB Web Storage limit).
pub const STORAGE_QUOTA_BYTES: usize = 5 * 1024 * 1024;

/// Storage area selector (`localStorage` vs `sessionStorage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageAreaKind {
    /// Persistent `localStorage`.
    Local,
    /// Tab-scoped `sessionStorage`.
    Session,
}

/// A document origin (`scheme + host + port`); partitions isolate data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin(String);

impl Origin {
    /// A new origin key (already normalized to `scheme://host:port` by the
    /// caller).
    pub fn new(origin: &str) -> Self {
        Self(origin.to_string())
    }
}

/// Failures writing to a storage area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The write would exceed [`STORAGE_QUOTA_BYTES`].
    QuotaExceeded,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuotaExceeded => write!(f, "storage quota exceeded"),
        }
    }
}

impl std::error::Error for StorageError {}

/// One entry change, feeding the future `storage` event dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageChange {
    /// Which area changed.
    pub area: StorageAreaKind,
    /// Changed key (`None` for `clear()`).
    pub key: Option<String>,
    /// Previous value, if any.
    pub old_value: Option<String>,
    /// New value (`None` for removals and `clear()`).
    pub new_value: Option<String>,
}

/// One Web Storage area: insertion-ordered string pairs under a quota.
///
/// Entries live in a `Vec` (linear scan) rather than a map on purpose:
/// `key(n)` is defined by insertion order, and Phase-0 areas stay small.
#[derive(Debug, Default, Clone)]
pub struct StorageArea {
    entries: Vec<(String, String)>,
    used_bytes: usize,
}

impl StorageArea {
    /// An empty area.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored pairs (`length`).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the area holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `getItem`: a clone of the value, if present.
    pub fn get(&self, key: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.clone())
    }

    /// `setItem`: insert or replace, enforcing the quota. Overwrites keep
    /// their original position (per spec, `setItem` on an existing key does
    /// not change its `key(n)` index).
    pub fn set(
        &mut self,
        area: StorageAreaKind,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        // The quota counts keys and values (UTF-8 bytes); overwrites keep
        // their key, so only the value delta counts then.
        let old_total = self.get(key).map(|old| key.len() + old.len()).unwrap_or(0);
        let new_total = key.len() + value.len();
        if self.used_bytes.saturating_sub(old_total) + new_total > STORAGE_QUOTA_BYTES {
            return Err(StorageError::QuotaExceeded);
        }
        let old_value =
            if let Some(slot) = self.entries.iter_mut().find(|(stored, _)| stored == key) {
                let old = std::mem::replace(&mut slot.1, value.to_string());
                Some(old)
            } else {
                self.entries.push((key.to_string(), value.to_string()));
                None
            };
        self.used_bytes = self.used_bytes.saturating_sub(old_total) + new_total;
        Ok(Some(StorageChange {
            area,
            key: Some(key.to_string()),
            old_value,
            new_value: Some(value.to_string()),
        }))
    }

    /// `removeItem`. `None` when the key was absent (no change, no event).
    pub fn remove(&mut self, area: StorageAreaKind, key: &str) -> Option<StorageChange> {
        let index = self.entries.iter().position(|(stored, _)| stored == key)?;
        let (_, old) = self.entries.remove(index);
        self.used_bytes -= key.len() + old.len();
        Some(StorageChange {
            area,
            key: Some(key.to_string()),
            old_value: Some(old),
            new_value: None,
        })
    }

    /// `key(n)`: the nth key in insertion order.
    pub fn key(&self, index: usize) -> Option<String> {
        self.entries.get(index).map(|(key, _)| key.clone())
    }

    /// `clear()`: drop everything, crediting the full byte count.
    pub fn clear(&mut self, area: StorageAreaKind) -> Option<StorageChange> {
        if self.entries.is_empty() {
            return None;
        }
        self.entries.clear();
        self.used_bytes = 0;
        Some(StorageChange {
            area,
            key: None,
            old_value: None,
            new_value: None,
        })
    }
}

/// Both areas for one origin.
#[derive(Debug, Default)]
pub struct StoragePartition {
    /// Persistent area.
    pub local: StorageArea,
    /// Tab-scoped area.
    pub session: StorageArea,
}

/// Origin-partitioned web storage with a change feed.
#[derive(Debug, Default)]
pub struct WebStorage {
    partitions: HashMap<Origin, StoragePartition>,
    changes: Vec<StorageChange>,
}

impl WebStorage {
    /// Empty storage.
    pub fn new() -> Self {
        Self::default()
    }

    fn area_mut(&mut self, origin: &Origin, area: StorageAreaKind) -> &mut StorageArea {
        let partition = self.partitions.entry(origin.clone()).or_default();
        match area {
            StorageAreaKind::Local => &mut partition.local,
            StorageAreaKind::Session => &mut partition.session,
        }
    }

    /// `getItem` on an origin's area.
    pub fn get(&self, origin: &Origin, area: StorageAreaKind, key: &str) -> Option<String> {
        let partition = self.partitions.get(origin)?;
        match area {
            StorageAreaKind::Local => partition.local.get(key),
            StorageAreaKind::Session => partition.session.get(key),
        }
    }

    /// `setItem` on an origin's area, recording the change.
    pub fn set(
        &mut self,
        origin: &Origin,
        area: StorageAreaKind,
        key: &str,
        value: &str,
    ) -> Result<(), StorageError> {
        let change = self.area_mut(origin, area).set(area, key, value)?;
        self.changes.extend(change);
        Ok(())
    }

    /// `removeItem` on an origin's area, recording the change.
    pub fn remove(&mut self, origin: &Origin, area: StorageAreaKind, key: &str) {
        let change = self.area_mut(origin, area).remove(area, key);
        self.changes.extend(change);
    }

    /// `clear()` on an origin's area, recording the change.
    pub fn clear(&mut self, origin: &Origin, area: StorageAreaKind) {
        let change = self.area_mut(origin, area).clear(area);
        self.changes.extend(change);
    }

    /// Drain recorded changes in order (future `storage` event dispatch).
    pub fn take_changes(&mut self) -> Vec<StorageChange> {
        std::mem::take(&mut self.changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::new("https://example.com")
    }

    #[test]
    fn crud_and_key_order() {
        let mut storage = WebStorage::new();
        storage
            .set(&origin(), StorageAreaKind::Local, "b", "2")
            .expect("set");
        storage
            .set(&origin(), StorageAreaKind::Local, "a", "1")
            .expect("set");
        assert_eq!(
            storage
                .get(&origin(), StorageAreaKind::Local, "a")
                .as_deref(),
            Some("1")
        );
        // Overwrite keeps its original index.
        storage
            .set(&origin(), StorageAreaKind::Local, "b", "3")
            .expect("set");
        let partition = storage.partitions.get(&origin()).expect("partition");
        assert_eq!(partition.local.key(0).as_deref(), Some("b"));
        assert_eq!(partition.local.key(1).as_deref(), Some("a"));
        assert_eq!(partition.local.key(2), None);
        assert_eq!(partition.local.len(), 2);
    }

    #[test]
    fn origins_and_areas_isolate() {
        let mut storage = WebStorage::new();
        let other = Origin::new("https://other.test");
        storage
            .set(&origin(), StorageAreaKind::Local, "k", "local")
            .expect("set");
        storage
            .set(&origin(), StorageAreaKind::Session, "k", "session")
            .expect("set");
        assert_eq!(storage.get(&other, StorageAreaKind::Local, "k"), None);
        assert_eq!(
            storage
                .get(&origin(), StorageAreaKind::Session, "k")
                .as_deref(),
            Some("session")
        );
        assert_eq!(
            storage
                .get(&origin(), StorageAreaKind::Local, "k")
                .as_deref(),
            Some("local")
        );
    }

    #[test]
    fn quota_rejects_oversize_writes() {
        let mut storage = WebStorage::new();
        let big = "x".repeat(STORAGE_QUOTA_BYTES + 1);
        assert_eq!(
            storage.set(&origin(), StorageAreaKind::Local, "big", &big),
            Err(StorageError::QuotaExceeded)
        );
        assert_eq!(storage.get(&origin(), StorageAreaKind::Local, "big"), None);
    }

    #[test]
    fn remove_and_clear_record_changes() {
        let mut storage = WebStorage::new();
        storage
            .set(&origin(), StorageAreaKind::Session, "k", "v")
            .expect("set");
        storage.remove(&origin(), StorageAreaKind::Session, "k");
        storage.remove(&origin(), StorageAreaKind::Session, "missing");
        storage
            .set(&origin(), StorageAreaKind::Session, "j", "w")
            .expect("set");
        storage.clear(&origin(), StorageAreaKind::Session);
        storage.clear(&origin(), StorageAreaKind::Session);
        let changes = storage.take_changes();
        assert_eq!(
            changes.len(),
            4,
            "only real mutations record, got {changes:?}"
        );
        assert_eq!(changes[0].old_value.as_deref(), None);
        assert_eq!(changes[0].new_value.as_deref(), Some("v"));
        assert_eq!(changes[1].key.as_deref(), Some("k"));
        assert_eq!(changes[1].new_value, None, "removal clears the value");
        assert_eq!(changes[3].key, None, "clear() has no key");
        assert_eq!(changes[3].area, StorageAreaKind::Session);
        assert!(storage.take_changes().is_empty(), "drain empties the feed");
    }
}
