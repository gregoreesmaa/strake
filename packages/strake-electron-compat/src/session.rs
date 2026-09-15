//! Electron `session` registry (`session.fromPath`/`fromPartition`,
//! issue #155).
//!
//! Records one [`Session`] per profile directory (`fromPath`), per named
//! partition (`fromPartition`), plus the shared default session. Repeated
//! calls with the same path or partition return the same session, matching
//! Electron/Chromium profile sharing. `protocol.handle` scheme names are
//! recorded per session; dispatch to the JS handler stays follow-up work
//! (no renderer network stack consumes them yet).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Opaque session handle, mirroring the bare-`u32` window ids. Id `0` is
/// always the default session.
pub type SessionId = u32;

/// Unknown session id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// No session with this id exists in the registry.
    UnknownSession(SessionId),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSession(id) => write!(f, "no session with id {id}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// One Electron `Session`: a profile directory, a named partition, or the
/// shared default session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    id: SessionId,
    path: Option<PathBuf>,
    partition: Option<String>,
    cache: bool,
    handled_schemes: Vec<String>,
}

impl Session {
    /// Session id.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Profile directory (`fromPath`), if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Partition name (`fromPartition`), if any.
    pub fn partition(&self) -> Option<&str> {
        self.partition.as_deref()
    }

    /// Whether the session persists its cache (`{ cache }` option).
    pub fn cache(&self) -> bool {
        self.cache
    }

    /// Schemes registered via `session.protocol.handle`, in order.
    pub fn handled_schemes(&self) -> &[String] {
        &self.handled_schemes
    }
}

/// Registry backing `session.fromPath`/`fromPartition`/`defaultSession`.
#[derive(Clone, Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<SessionId, Session>,
    by_path: HashMap<PathBuf, SessionId>,
    by_partition: HashMap<String, SessionId>,
    next_id: SessionId,
}

impl SessionRegistry {
    /// Empty registry with the default session pre-created as id `0`.
    pub fn new() -> Self {
        let mut registry = Self::default();
        registry.sessions.insert(
            0,
            Session {
                id: 0,
                path: None,
                partition: None,
                cache: true,
                handled_schemes: Vec::new(),
            },
        );
        registry.next_id = 1;
        registry
    }

    /// The shared default session (`session.defaultSession`).
    pub fn default_session(&self) -> SessionId {
        0
    }

    /// Look up a session by id.
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    /// Session ids in creation order.
    pub fn ids(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// `session.fromPath(path, { cache })`: the session for a profile
    /// directory, creating it on first use. Repeats with the same path
    /// return the same session.
    pub fn from_path(&mut self, path: &Path, cache: bool) -> SessionId {
        if let Some(id) = self.by_path.get(path) {
            return *id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.by_path.insert(path.to_path_buf(), id);
        self.sessions.insert(
            id,
            Session {
                id,
                path: Some(path.to_path_buf()),
                partition: None,
                cache,
                handled_schemes: Vec::new(),
            },
        );
        id
    }

    /// `session.fromPartition(partition, { cache })`: the session for a
    /// named partition, creating it on first use. Repeats with the same
    /// name return the same session.
    pub fn from_partition(&mut self, partition: &str, cache: bool) -> SessionId {
        if let Some(id) = self.by_partition.get(partition) {
            return *id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.by_partition.insert(partition.to_string(), id);
        self.sessions.insert(
            id,
            Session {
                id,
                path: None,
                partition: Some(partition.to_string()),
                cache,
                handled_schemes: Vec::new(),
            },
        );
        id
    }

    /// Record a `session.protocol.handle(scheme, …)` registration.
    /// Re-handling an already-handled scheme is last-wins, matching the
    /// privileged-scheme registry stance.
    pub fn record_protocol_handler(
        &mut self,
        id: SessionId,
        scheme: &str,
    ) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(SessionError::UnknownSession(id))?;
        if !session.handled_schemes.iter().any(|s| s == scheme) {
            session.handled_schemes.push(scheme.to_string());
        }
        Ok(())
    }
}

#[test]
fn default_session_is_id_zero() {
    let registry = SessionRegistry::new();
    assert_eq!(registry.default_session(), 0);
    assert_eq!(registry.ids(), vec![0]);
    let default = registry.get(0).expect("default session exists");
    assert_eq!(default.path(), None);
    assert_eq!(default.partition(), None);
    assert!(default.handled_schemes().is_empty());
}

#[test]
fn from_path_dedups_by_directory() {
    let mut registry = SessionRegistry::new();
    let first = registry.from_path(Path::new("/tmp/profile/internal"), false);
    assert_ne!(first, 0);
    assert_eq!(
        registry.from_path(Path::new("/tmp/profile/internal"), false),
        first
    );
    let other = registry.from_path(Path::new("/tmp/profile/other"), false);
    assert_ne!(other, first);
    let session = registry.get(first).expect("session exists");
    assert_eq!(session.path(), Some(Path::new("/tmp/profile/internal")));
    assert!(!session.cache());
    assert_eq!(registry.ids().len(), 3);
}

#[test]
fn from_partition_dedups_by_name() {
    let mut registry = SessionRegistry::new();
    let first = registry.from_partition("electron-updater", false);
    assert_ne!(first, 0);
    assert_eq!(registry.from_partition("electron-updater", false), first);
    let session = registry.get(first).expect("session exists");
    assert_eq!(session.partition(), Some("electron-updater"));
}

#[test]
fn protocol_handler_recording_is_last_wins_per_scheme() {
    let mut registry = SessionRegistry::new();
    let id = registry.from_path(Path::new("/tmp/profile/internal"), false);
    registry
        .record_protocol_handler(id, "joplin-content")
        .expect("known session");
    registry
        .record_protocol_handler(id, "joplin-plugin")
        .expect("known session");
    registry
        .record_protocol_handler(id, "joplin-content")
        .expect("re-handling records once");
    assert_eq!(
        registry.get(id).expect("session exists").handled_schemes(),
        &["joplin-content".to_string(), "joplin-plugin".to_string()]
    );
    assert_eq!(
        registry.record_protocol_handler(999, "x"),
        Err(SessionError::UnknownSession(999))
    );
    assert_eq!(registry.get(999), None);
}
