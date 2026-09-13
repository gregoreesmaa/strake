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
//! `StorageBackend` seam next; the stores themselves are backend-agnostic.

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
    /// A new origin key, normalized so textual variants of one origin share
    /// a partition: the scheme and host are lowercased, default ports
    /// (`:80` for `http`, `:443` for `https`) are dropped, and any
    /// path/query/fragment is stripped (an origin is scheme + host + port).
    /// Inputs without a `scheme://` prefix are lowercased verbatim.
    pub fn new(origin: &str) -> Self {
        Self(normalize_origin(origin))
    }
}

/// Lowercase scheme/host, drop default ports, strip path and below.
///
/// Kept dependency-free on purpose (no URL crate in this layer): it covers
/// `scheme://authority[/path][?query][#fragment]` plus bare hosts, IPv6
/// literals in brackets, and explicit ports. Anything unrecognized falls
/// back to a trimmed lowercase copy rather than failing.
fn normalize_origin(origin: &str) -> String {
    let origin = origin.trim();
    let Some(scheme_end) = origin.find("://") else {
        return origin.to_lowercase();
    };
    let scheme = origin[..scheme_end].to_lowercase();
    let rest = &origin[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // Split host from port without breaking IPv6 literals.
    let (host, port, bracketed) = if let Some(stripped) = authority.strip_prefix('[') {
        match stripped.find(']') {
            Some(close) => {
                let host = &stripped[..close];
                let port = stripped[close + 1..].strip_prefix(':');
                (host, port, true)
            }
            None => (authority, None, false),
        }
    } else {
        match authority.rfind(':') {
            Some(colon) if authority[colon + 1..].bytes().all(|b| b.is_ascii_digit()) => {
                (&authority[..colon], Some(&authority[colon + 1..]), false)
            }
            _ => (authority, None, false),
        }
    };
    let host = if bracketed {
        format!("[{}]", host.to_lowercase())
    } else {
        host.to_lowercase()
    };
    let default_port = matches!(
        (scheme.as_str(), port),
        ("http", Some("80")) | ("https", Some("443"))
    );
    match port {
        Some(port) if !default_port && !port.is_empty() => format!("{scheme}://{host}:{port}"),
        _ => format!("{scheme}://{host}"),
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
    ) -> Result<StorageChange, StorageError> {
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
        Ok(StorageChange {
            area,
            key: Some(key.to_string()),
            old_value,
            new_value: Some(value.to_string()),
        })
    }

    /// `removeItem`. `None` when the key was absent (no change, no event).
    pub fn remove(&mut self, area: StorageAreaKind, key: &str) -> Option<StorageChange> {
        let index = self.entries.iter().position(|(stored, _)| stored == key)?;
        let (_, old) = self.entries.remove(index);
        // Defensive `saturating_sub` (mirroring `set`): `used_bytes` must
        // equal the entry-size sum, but a future path that touches `entries`
        // without updating the counter (backend restore, cross-window sync)
        // must degrade to 0, never underflow-panic in debug or wrap in
        // release.
        self.used_bytes = self.used_bytes.saturating_sub(key.len() + old.len());
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

    fn area(&self, origin: &Origin, area: StorageAreaKind) -> Option<&StorageArea> {
        let partition = self.partitions.get(origin)?;
        match area {
            StorageAreaKind::Local => Some(&partition.local),
            StorageAreaKind::Session => Some(&partition.session),
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
    ///
    /// A quota-rejected write on an otherwise-empty partition prunes the
    /// just-created partition instead of leaving an empty entry behind.
    pub fn set(
        &mut self,
        origin: &Origin,
        area: StorageAreaKind,
        key: &str,
        value: &str,
    ) -> Result<(), StorageError> {
        let result = self.area_mut(origin, area).set(area, key, value);
        match result {
            Ok(change) => {
                self.changes.push(change);
                Ok(())
            }
            Err(error) => {
                if self.partitions.get(origin).is_some_and(|partition| {
                    partition.local.is_empty() && partition.session.is_empty()
                }) {
                    self.partitions.remove(origin);
                }
                Err(error)
            }
        }
    }

    /// `removeItem` on an origin's area, recording the change.
    ///
    /// A missing key — or a missing partition — records nothing and inserts
    /// nothing.
    pub fn remove(&mut self, origin: &Origin, area: StorageAreaKind, key: &str) {
        let Some(partition) = self.partitions.get_mut(origin) else {
            return;
        };
        let slot = match area {
            StorageAreaKind::Local => &mut partition.local,
            StorageAreaKind::Session => &mut partition.session,
        };
        let change = slot.remove(area, key);
        self.changes.extend(change);
    }

    /// `clear()` on an origin's area, recording the change.
    ///
    /// Clearing an empty or missing area records nothing and inserts
    /// nothing.
    pub fn clear(&mut self, origin: &Origin, area: StorageAreaKind) {
        let Some(partition) = self.partitions.get_mut(origin) else {
            return;
        };
        let slot = match area {
            StorageAreaKind::Local => &mut partition.local,
            StorageAreaKind::Session => &mut partition.session,
        };
        let change = slot.clear(area);
        self.changes.extend(change);
    }

    /// `length` for an origin's area: 0 when the partition is absent.
    ///
    /// Reads through a shared `get` lookup so a missing partition is never
    /// inserted — the JS `localStorage.length` binding calls this.
    pub fn len(&self, origin: &Origin, area: StorageAreaKind) -> usize {
        self.area(origin, area).map_or(0, StorageArea::len)
    }

    /// Whether an origin's area holds nothing (absent partitions count as
    /// empty).
    pub fn is_empty(&self, origin: &Origin, area: StorageAreaKind) -> bool {
        self.len(origin, area) == 0
    }

    /// `key(n)` for an origin's area in insertion order: `None` when the
    /// partition is absent or `n` is out of range, without inserting.
    pub fn key(&self, origin: &Origin, area: StorageAreaKind, index: usize) -> Option<String> {
        self.area(origin, area)?.key(index)
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
        // Overwrite keeps its original index (via the public `WebStorage`
        // surface, the same path the JS `length` / `key(n)` binding uses).
        storage
            .set(&origin(), StorageAreaKind::Local, "b", "3")
            .expect("set");
        assert_eq!(
            storage.key(&origin(), StorageAreaKind::Local, 0).as_deref(),
            Some("b")
        );
        assert_eq!(
            storage.key(&origin(), StorageAreaKind::Local, 1).as_deref(),
            Some("a")
        );
        assert_eq!(storage.key(&origin(), StorageAreaKind::Local, 2), None);
        assert_eq!(storage.len(&origin(), StorageAreaKind::Local), 2);
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

    #[test]
    fn origin_variants_share_one_partition() {
        for spelling in [
            "https://example.com",
            "https://example.com:443",
            "HTTPS://EXAMPLE.COM",
            "https://example.com:443/some/path?query#fragment",
            "  https://Example.COM  ",
        ] {
            assert_eq!(
                Origin::new(spelling),
                origin(),
                "spelling shares the partition: {spelling}"
            );
        }
        // Non-default ports, schemes, and hosts stay isolated.
        assert_ne!(Origin::new("https://example.com:8443"), origin());
        assert_ne!(Origin::new("http://example.com"), origin());
        assert_ne!(Origin::new("http://example.com:80"), origin());
        assert_eq!(
            Origin::new("http://example.com"),
            Origin::new("http://example.com:80"),
            "http default port drops too"
        );
        assert_ne!(Origin::new("https://other.test"), origin());
        // Data written under one spelling reads back under the others.
        let mut storage = WebStorage::new();
        storage
            .set(
                &Origin::new("HTTPS://EXAMPLE.COM:443"),
                StorageAreaKind::Local,
                "k",
                "v",
            )
            .expect("set");
        assert_eq!(
            storage
                .get(
                    &Origin::new("https://example.com"),
                    StorageAreaKind::Local,
                    "k"
                )
                .as_deref(),
            Some("v")
        );
    }

    #[test]
    fn length_and_key_read_absent_partitions_without_inserting() {
        let storage = WebStorage::new();
        let missing = Origin::new("https://missing.test");
        assert_eq!(storage.len(&missing, StorageAreaKind::Local), 0);
        assert!(storage.is_empty(&missing, StorageAreaKind::Session));
        assert_eq!(storage.key(&missing, StorageAreaKind::Local, 0), None);
        assert_eq!(storage.get(&missing, StorageAreaKind::Local, "k"), None);
        assert!(
            !storage.partitions.contains_key(&missing),
            "reads must not insert partitions"
        );
    }

    #[test]
    fn noop_mutations_leave_no_partition_behind() {
        let mut storage = WebStorage::new();
        let missing = Origin::new("https://missing.test");
        storage.remove(&missing, StorageAreaKind::Local, "nope");
        storage.clear(&missing, StorageAreaKind::Local);
        storage.clear(&missing, StorageAreaKind::Session);
        // A quota-rejected write must not leave an empty partition either.
        let big = "x".repeat(STORAGE_QUOTA_BYTES + 1);
        assert_eq!(
            storage.set(&missing, StorageAreaKind::Local, "big", &big),
            Err(StorageError::QuotaExceeded)
        );
        assert!(
            !storage.partitions.contains_key(&missing),
            "no-op mutations must not insert partitions"
        );
        assert!(storage.take_changes().is_empty());
        // And the public reads still report empty without inserting.
        assert_eq!(storage.len(&missing, StorageAreaKind::Local), 0);
        assert_eq!(storage.key(&missing, StorageAreaKind::Local, 0), None);
    }

    #[test]
    fn quota_credits_remove_overwrite_shrink_and_clear() {
        let mut storage = WebStorage::new();
        // Fill to 10 bytes under quota; the same-shaped write then fails.
        let filler = "v".repeat(STORAGE_QUOTA_BYTES - "filler".len() - 10);
        storage
            .set(&origin(), StorageAreaKind::Local, "filler", &filler)
            .expect("near-quota set");
        let blocked_value = "v".repeat(11);
        assert_eq!(
            storage.set(&origin(), StorageAreaKind::Local, "extra", &blocked_value),
            Err(StorageError::QuotaExceeded)
        );
        // `remove` frees the bytes: the same write succeeds again.
        storage.remove(&origin(), StorageAreaKind::Local, "filler");
        storage
            .set(&origin(), StorageAreaKind::Local, "extra", &blocked_value)
            .expect("remove credits quota");
        // Overwrite-with-smaller frees the delta: a near-quota refill fits.
        let refill = "w".repeat(STORAGE_QUOTA_BYTES - "extra".len() - blocked_value.len() - 1);
        storage
            .set(&origin(), StorageAreaKind::Local, "extra", "s")
            .expect("shrink");
        storage
            .set(&origin(), StorageAreaKind::Local, "refill", &refill)
            .expect("overwrite-shrink credits quota");
        assert_eq!(
            storage
                .get(&origin(), StorageAreaKind::Local, "refill")
                .as_deref(),
            Some(refill.as_str())
        );
        // `clear` frees everything: the full-size write fits again.
        storage.clear(&origin(), StorageAreaKind::Local);
        let full = "z".repeat(STORAGE_QUOTA_BYTES - "full".len());
        storage
            .set(&origin(), StorageAreaKind::Local, "full", &full)
            .expect("clear credits quota");
    }

    #[test]
    fn remove_with_stale_counter_saturates_instead_of_underflowing() {
        // Simulates a future path (backend restore, cross-window sync) that
        // touches `entries` without updating the counter: the accounting
        // must degrade to 0, never underflow (panic in debug, wrap in
        // release).
        let mut area = StorageArea::new();
        area.set(StorageAreaKind::Local, "k", "value").expect("set");
        area.used_bytes = 0;
        let change = area.remove(StorageAreaKind::Local, "k");
        assert!(change.is_some(), "entry is still removed");
        assert_eq!(area.used_bytes, 0);
        assert!(area.is_empty());
    }
}
