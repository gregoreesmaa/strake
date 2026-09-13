//! Headless Electron `shell` shim (issue #21 step 2).
//!
//! `shell.openExternal` / `showItemInFolder` / `beep` need an injectable
//! seam before any OS integration exists: URL validation (a scheme is
//! required — a bare path is never a valid external target) plus a
//! [`OsShellBackend`] trait the runtime implements. Production delivery
//! (desktop-entry launch on Linux, `open(1)` on macOS, `ShellExecute` on
//! Windows, per #12's OS-integration bullet) binds this trait in a
//! follow-up; CI and headless tests use the [`RecordingOsShell`]
//! pass-through, which records every request so external-navigation flows
//! stay fully testable without launching other apps.

use std::sync::{Arc, Mutex};

/// `shell.openExternal` / `showItemInFolder` failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsShellError {
    /// The URL has no `scheme:` prefix (or is empty).
    InvalidUrl(String),
    /// The path is empty.
    InvalidPath(String),
    /// The backend refused the request.
    Denied(String),
}

impl std::fmt::Display for OsShellError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl(url) => write!(formatter, "invalid external URL: {url}"),
            Self::InvalidPath(path) => write!(formatter, "invalid path: {path}"),
            Self::Denied(reason) => write!(formatter, "shell request denied: {reason}"),
        }
    }
}

impl std::error::Error for OsShellError {}

/// OS shell backend. The trait boundary is the whole "leave the app" flow:
/// real platforms launch the OS handler here, while [`RecordingOsShell`]
/// stands in headless.
pub trait OsShellBackend: Send + Sync {
    /// Open a URL in the OS default handler.
    fn open_external(&self, url: &str) -> Result<(), OsShellError>;
    /// Reveal a path in the OS file manager.
    fn show_item_in_folder(&self, path: &str) -> Result<(), OsShellError>;
    /// Play the OS beep.
    fn beep(&self);
    /// External URLs requested so far, in order.
    fn opened_urls(&self) -> Vec<String>;
    /// Revealed paths so far, in order.
    fn shown_paths(&self) -> Vec<String>;
    /// Beep count.
    fn beeps(&self) -> u64;
}

#[derive(Debug, Default)]
struct RecordingState {
    opened_urls: Vec<String>,
    shown_paths: Vec<String>,
    beeps: u64,
}

/// Headless pass-through: validates, records, and succeeds (issue #21
/// acceptance: external navigation resolves without leaving the test).
#[derive(Debug, Default)]
pub struct RecordingOsShell {
    state: Mutex<RecordingState>,
}

impl RecordingOsShell {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `url` carries a `scheme:` prefix.
    fn has_scheme(url: &str) -> bool {
        match url.split_once(':') {
            Some((scheme, _)) => {
                !scheme.is_empty()
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
            }
            None => false,
        }
    }
}

impl OsShellBackend for RecordingOsShell {
    fn open_external(&self, url: &str) -> Result<(), OsShellError> {
        if !Self::has_scheme(url) {
            return Err(OsShellError::InvalidUrl(url.to_string()));
        }
        self.state
            .lock()
            .expect("os-shell mutex")
            .opened_urls
            .push(url.to_string());
        Ok(())
    }

    fn show_item_in_folder(&self, path: &str) -> Result<(), OsShellError> {
        if path.is_empty() {
            return Err(OsShellError::InvalidPath(path.to_string()));
        }
        self.state
            .lock()
            .expect("os-shell mutex")
            .shown_paths
            .push(path.to_string());
        Ok(())
    }

    fn beep(&self) {
        self.state.lock().expect("os-shell mutex").beeps += 1;
    }

    fn opened_urls(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("os-shell mutex")
            .opened_urls
            .clone()
    }

    fn shown_paths(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("os-shell mutex")
            .shown_paths
            .clone()
    }

    fn beeps(&self) -> u64 {
        self.state.lock().expect("os-shell mutex").beeps
    }
}

/// Main-process `shell` hub (`shell.openExternal(...)` and friends).
#[derive(Clone)]
pub struct OsShell {
    backend: Arc<dyn OsShellBackend>,
}

impl OsShell {
    /// Serve shell calls through an explicit backend (OS handlers at
    /// runtime, recorder in tests).
    pub fn new(backend: Arc<dyn OsShellBackend>) -> Self {
        Self { backend }
    }

    /// Headless/CI hub backed by [`RecordingOsShell`].
    pub fn recording() -> Self {
        Self::new(Arc::new(RecordingOsShell::new()))
    }

    /// `shell.openExternal(url)`.
    pub fn open_external(&self, url: &str) -> Result<(), OsShellError> {
        self.backend.open_external(url)
    }

    /// `shell.showItemInFolder(fullPath)`.
    pub fn show_item_in_folder(&self, path: &str) -> Result<(), OsShellError> {
        self.backend.show_item_in_folder(path)
    }

    /// `shell.beep()`.
    pub fn beep(&self) {
        self.backend.beep();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_external_records_valid_urls() {
        let backend = Arc::new(RecordingOsShell::new());
        let shell = OsShell::new(backend.clone());
        shell
            .open_external("https://example.com/docs")
            .expect("https opens");
        shell
            .open_external("mailto:team@example.com")
            .expect("mailto opens");
        shell
            .open_external("myapp://deep/link")
            .expect("custom scheme opens");
        assert_eq!(
            backend.opened_urls(),
            vec![
                String::from("https://example.com/docs"),
                String::from("mailto:team@example.com"),
                String::from("myapp://deep/link"),
            ]
        );
    }

    #[test]
    fn open_external_rejects_schemeless_targets() {
        let shell = OsShell::recording();
        assert_eq!(
            shell.open_external("example.com/no-scheme"),
            Err(OsShellError::InvalidUrl(String::from(
                "example.com/no-scheme"
            )))
        );
        assert_eq!(
            shell.open_external(""),
            Err(OsShellError::InvalidUrl(String::new()))
        );
    }

    #[test]
    fn show_item_and_beep_record() {
        let backend = Arc::new(RecordingOsShell::new());
        let shell = OsShell::new(backend.clone());
        shell
            .show_item_in_folder("/docs/out.md")
            .expect("path reveals");
        assert_eq!(
            shell.show_item_in_folder(""),
            Err(OsShellError::InvalidPath(String::new()))
        );
        shell.beep();
        shell.beep();
        assert_eq!(backend.shown_paths(), vec![String::from("/docs/out.md")]);
        assert_eq!(backend.beeps(), 2);
    }
}
