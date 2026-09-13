//! `clipboard` over the native shell clipboard (issue #95).
//!
//! The OS clipboard already exists underneath: [`strake_traits::shell`]
//! exposes `get/set_clipboard_text` and `strake-shell` implements it with
//! `arboard` behind `feature = "clipboard"`. This module is the missing
//! Electron binding: [`Clipboard`] fronts a [`ClipboardBackend`] so headless
//! tests (and CI, where no system clipboard exists) inject the
//! [`MemoryClipboard`] recorder while the runtime wires the arboard backend.
//!
//! [`strake_traits::shell`]: https://github.com/gregoreesmaa/strake/blob/main/packages/strake-traits/src/shell.rs

use std::sync::{Arc, Mutex};

/// Failures reading or writing the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// No system clipboard is available (headless CI, denied permission).
    Unavailable,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "no system clipboard available"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// OS clipboard backend. The runtime implementation fronts `arboard` via
/// `strake-shell`; tests inject [`MemoryClipboard`].
pub trait ClipboardBackend: Send + Sync {
    /// Current text contents, or [`ClipboardError::Unavailable`].
    fn read_text(&self) -> Result<String, ClipboardError>;
    /// Replace the text contents.
    fn write_text(&self, text: &str) -> Result<(), ClipboardError>;
    /// Clear the contents.
    fn clear(&self) -> Result<(), ClipboardError>;
}

/// In-memory clipboard: the headless/CI recorder (issue #95 acceptance).
/// Never touches the OS clipboard, so tests stay hermetic.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    text: Mutex<Option<String>>,
}

impl MemoryClipboard {
    /// An empty in-memory clipboard.
    pub fn new() -> Self {
        Self::default()
    }

    /// Last written text, if any (test observation).
    pub fn last_written(&self) -> Option<String> {
        self.text.lock().expect("clipboard mutex").clone()
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn read_text(&self) -> Result<String, ClipboardError> {
        self.text
            .lock()
            .expect("clipboard mutex")
            .clone()
            .ok_or(ClipboardError::Unavailable)
    }

    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        *self.text.lock().expect("clipboard mutex") = Some(text.to_string());
        Ok(())
    }

    fn clear(&self) -> Result<(), ClipboardError> {
        *self.text.lock().expect("clipboard mutex") = None;
        Ok(())
    }
}

/// Electron `clipboard` (`readText`/`writeText`, plus `clear` where cheap).
#[derive(Clone)]
pub struct Clipboard {
    backend: Arc<dyn ClipboardBackend>,
}

impl Clipboard {
    /// Bind an explicit backend (arboard at runtime, memory in tests).
    pub fn new(backend: Arc<dyn ClipboardBackend>) -> Self {
        Self { backend }
    }

    /// Headless/CI clipboard backed by [`MemoryClipboard`].
    pub fn memory() -> Self {
        Self::new(Arc::new(MemoryClipboard::new()))
    }

    /// `clipboard.readText()`. `Err(Unavailable)` when empty/unavailable,
    /// matching Electron's empty-string-or-throw posture at the binding
    /// layer (the JS shim maps this to `""`).
    pub fn read_text(&self) -> Result<String, ClipboardError> {
        self.backend.read_text()
    }

    /// `clipboard.writeText(text)`.
    pub fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        self.backend.write_text(text)
    }

    /// `clipboard.clear()`.
    pub fn clear(&self) -> Result<(), ClipboardError> {
        self.backend.clear()
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::memory()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_clipboard_round_trips_and_clears() {
        let clipboard = Clipboard::memory();
        assert_eq!(clipboard.read_text(), Err(ClipboardError::Unavailable));
        clipboard.write_text("hello").expect("write");
        assert_eq!(clipboard.read_text().as_deref(), Ok("hello"));
        clipboard.clear().expect("clear");
        assert_eq!(clipboard.read_text(), Err(ClipboardError::Unavailable));
    }

    #[test]
    fn memory_backend_records_last_write() {
        let backend = Arc::new(MemoryClipboard::new());
        let clipboard = Clipboard::new(backend.clone());
        clipboard.write_text("x").expect("write");
        assert_eq!(backend.last_written().as_deref(), Some("x"));
    }
}
