//! Headless `dialog` shim (issues #21 step 2, #12 file-dialog bullet).
//!
//! `dialog.showOpenDialog` / `showSaveDialog` / `showMessageBox` need a
//! validated data model before any OS panel exists: option structs mirroring
//! Electron's signatures, result structs mirroring its return values, and a
//! [`DialogBackend`] trait the runtime implements. Production panels
//! (`rfd` async pickers on the UI thread, per #12's non-blocking
//! requirement) bind this trait in a follow-up; CI and headless tests use
//! the [`MemoryDialog`] stand-in, which records every call and replays
//! scripted responses so dialog-gated flows stay fully testable without an
//! OS.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// `dialog.showOpenDialog` options (Electron subset for the Day-1 shim).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenDialogOptions {
    /// Dialog title.
    pub title: Option<String>,
    /// Starting directory.
    pub default_path: Option<String>,
    /// `openFile`, `openDirectory`, `multiSelections`, `showHiddenFiles`,
    /// `createDirectory`, `promptToCreate`, `noResolveAliases`,
    /// `treatPackageAsDirectory`, `dontAddToRecent`.
    pub properties: Vec<OpenProperty>,
    /// Extension filters (`{ name, extensions }`).
    pub filters: Vec<FileFilter>,
}

/// One `properties` entry of [`OpenDialogOptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenProperty {
    /// Pick files.
    OpenFile,
    /// Pick directories.
    OpenDirectory,
    /// Allow more than one selection.
    MultiSelections,
    /// Show hidden files.
    ShowHiddenFiles,
    /// Offer a create-directory button.
    CreateDirectory,
}

/// One `{ name, extensions }` file filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    /// Human-readable group name.
    pub name: String,
    /// Extensions without the leading dot.
    pub extensions: Vec<String>,
}

/// `dialog.showOpenDialog` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDialogResult {
    /// Whether the user cancelled.
    pub canceled: bool,
    /// Selected paths (empty when cancelled).
    pub file_paths: Vec<String>,
}

/// `dialog.showSaveDialog` options (Electron subset).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SaveDialogOptions {
    /// Dialog title.
    pub title: Option<String>,
    /// Suggested file name or starting directory.
    pub default_path: Option<String>,
    /// Extension filters.
    pub filters: Vec<FileFilter>,
}

/// `dialog.showSaveDialog` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveDialogResult {
    /// Whether the user cancelled.
    pub canceled: bool,
    /// Chosen path (present unless cancelled).
    pub file_path: Option<String>,
}

/// `dialog.showMessageBox` options (Electron subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBoxOptions {
    /// `none`, `info`, `error`, `question`, `warning`.
    pub box_type: MessageBoxType,
    /// Button labels, in order (index is the response id).
    pub buttons: Vec<String>,
    /// Window title.
    pub title: Option<String>,
    /// Main message.
    pub message: String,
    /// Supplemental detail text.
    pub detail: Option<String>,
    /// Default-selected button index.
    pub default_id: usize,
    /// Button index treated as cancel.
    pub cancel_id: usize,
}

impl Default for MessageBoxOptions {
    fn default() -> Self {
        Self {
            box_type: MessageBoxType::None,
            buttons: vec![String::from("OK")],
            title: None,
            message: String::new(),
            detail: None,
            default_id: 0,
            cancel_id: 0,
        }
    }
}

/// Message-box severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageBoxType {
    /// Plain box.
    #[default]
    None,
    /// Informational.
    Info,
    /// Error.
    Error,
    /// Question.
    Question,
    /// Warning.
    Warning,
}

/// `dialog.showMessageBox` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBoxResult {
    /// Index of the clicked button.
    pub response: usize,
    /// Checkbox state (`false` when no checkbox was shown).
    pub checkbox_checked: bool,
}

/// Native dialog backend. The trait boundary is the whole OS-panel flow:
/// real platforms show non-blocking `rfd` panels here (never freezing the
/// render loop, per #12), while [`MemoryDialog`] stands in headless.
pub trait DialogBackend: Send + Sync {
    /// Show the open panel; `Ok` with `canceled: true` when dismissed.
    fn show_open_dialog(&self, options: OpenDialogOptions) -> OpenDialogResult;
    /// Show the save panel; `Ok` with `canceled: true` when dismissed.
    fn show_save_dialog(&self, options: SaveDialogOptions) -> SaveDialogResult;
    /// Show the message box, returning the clicked button index.
    fn show_message_box(&self, options: MessageBoxOptions) -> MessageBoxResult;
    /// Open-panel calls so far, in order.
    fn open_calls(&self) -> Vec<OpenDialogOptions>;
    /// Save-panel calls so far, in order.
    fn save_calls(&self) -> Vec<SaveDialogOptions>;
    /// Message-box calls so far, in order.
    fn message_calls(&self) -> Vec<MessageBoxOptions>;
}

/// Scripted responses for [`MemoryDialog`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScriptedDialog {
    /// Queued open-panel results (a missing entry means cancelled).
    pub open_results: VecDeque<OpenDialogResult>,
    /// Queued save-panel results (a missing entry means cancelled).
    pub save_results: VecDeque<SaveDialogResult>,
    /// Queued message-box button indexes (a missing entry means 0).
    pub message_responses: VecDeque<usize>,
}

#[derive(Debug, Default)]
struct MemoryState {
    script: ScriptedDialog,
    open_calls: Vec<OpenDialogOptions>,
    save_calls: Vec<SaveDialogOptions>,
    message_calls: Vec<MessageBoxOptions>,
}

/// Headless pass-through: records calls and replays scripted responses
/// (issue #21 acceptance: dialog calls resolve without an OS panel).
#[derive(Debug, Default)]
pub struct MemoryDialog {
    state: Mutex<MemoryState>,
}

impl MemoryDialog {
    /// An empty recorder (every panel reports cancelled / button 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// A recorder preloaded with scripted responses.
    pub fn scripted(script: ScriptedDialog) -> Self {
        Self {
            state: Mutex::new(MemoryState {
                script,
                open_calls: Vec::new(),
                save_calls: Vec::new(),
                message_calls: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state.lock().expect("dialog mutex")
    }
}

impl DialogBackend for MemoryDialog {
    fn show_open_dialog(&self, options: OpenDialogOptions) -> OpenDialogResult {
        let mut state = self.lock();
        state.open_calls.push(options);
        state
            .script
            .open_results
            .pop_front()
            .unwrap_or(OpenDialogResult {
                canceled: true,
                file_paths: Vec::new(),
            })
    }

    fn show_save_dialog(&self, options: SaveDialogOptions) -> SaveDialogResult {
        let mut state = self.lock();
        state.save_calls.push(options);
        state
            .script
            .save_results
            .pop_front()
            .unwrap_or(SaveDialogResult {
                canceled: true,
                file_path: None,
            })
    }

    fn show_message_box(&self, options: MessageBoxOptions) -> MessageBoxResult {
        let mut state = self.lock();
        state.message_calls.push(options);
        MessageBoxResult {
            response: state.script.message_responses.pop_front().unwrap_or(0),
            checkbox_checked: false,
        }
    }

    fn open_calls(&self) -> Vec<OpenDialogOptions> {
        self.lock().open_calls.clone()
    }

    fn save_calls(&self) -> Vec<SaveDialogOptions> {
        self.lock().save_calls.clone()
    }

    fn message_calls(&self) -> Vec<MessageBoxOptions> {
        self.lock().message_calls.clone()
    }
}

/// Main-process `dialog` hub (`dialog.show*` from the renderer side).
#[derive(Clone)]
pub struct Dialog {
    backend: Arc<dyn DialogBackend>,
}

impl Dialog {
    /// Serve dialogs through an explicit backend (OS panels at runtime,
    /// recorder in tests).
    pub fn new(backend: Arc<dyn DialogBackend>) -> Self {
        Self { backend }
    }

    /// Headless/CI hub backed by [`MemoryDialog`].
    pub fn memory() -> Self {
        Self::new(Arc::new(MemoryDialog::new()))
    }

    /// `dialog.showOpenDialog(options)`.
    pub fn show_open_dialog(&self, options: OpenDialogOptions) -> OpenDialogResult {
        self.backend.show_open_dialog(options)
    }

    /// `dialog.showSaveDialog(options)`.
    pub fn show_save_dialog(&self, options: SaveDialogOptions) -> SaveDialogResult {
        self.backend.show_save_dialog(options)
    }

    /// `dialog.showMessageBox(options)`.
    pub fn show_message_box(&self, options: MessageBoxOptions) -> MessageBoxResult {
        self.backend.show_message_box(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_script() -> ScriptedDialog {
        ScriptedDialog {
            open_results: VecDeque::from([OpenDialogResult {
                canceled: false,
                file_paths: vec![String::from("/docs/notes.md")],
            }]),
            save_results: VecDeque::from([SaveDialogResult {
                canceled: false,
                file_path: Some(String::from("/docs/out.md")),
            }]),
            message_responses: VecDeque::from([1]),
        }
    }

    #[test]
    fn open_dialog_records_call_and_replays_paths() {
        let backend = Arc::new(MemoryDialog::scripted(open_script()));
        let dialog = Dialog::new(backend.clone());
        let options = OpenDialogOptions {
            properties: vec![OpenProperty::OpenFile],
            ..OpenDialogOptions::default()
        };
        let result = dialog.show_open_dialog(options.clone());
        assert!(!result.canceled);
        assert_eq!(result.file_paths, vec![String::from("/docs/notes.md")]);
        assert_eq!(backend.open_calls(), vec![options]);
    }

    #[test]
    fn dismissed_panels_report_cancelled() {
        let dialog = Dialog::memory();
        let open = dialog.show_open_dialog(OpenDialogOptions::default());
        assert!(open.canceled);
        assert!(open.file_paths.is_empty());
        let save = dialog.show_save_dialog(SaveDialogOptions::default());
        assert!(save.canceled);
        assert_eq!(save.file_path, None);
    }

    #[test]
    fn message_box_replays_button_index() {
        let backend = Arc::new(MemoryDialog::scripted(open_script()));
        let dialog = Dialog::new(backend.clone());
        let options = MessageBoxOptions {
            box_type: MessageBoxType::Question,
            buttons: vec![String::from("No"), String::from("Yes")],
            message: String::from("Save first?"),
            ..MessageBoxOptions::default()
        };
        let result = dialog.show_message_box(options.clone());
        assert_eq!(
            result,
            MessageBoxResult {
                response: 1,
                checkbox_checked: false,
            }
        );
        assert_eq!(backend.message_calls(), vec![options]);
    }

    #[test]
    fn scripted_queues_drain_fifo() {
        let dialog = Dialog::new(Arc::new(MemoryDialog::scripted(open_script())));
        assert!(
            !dialog
                .show_save_dialog(SaveDialogOptions::default())
                .canceled
        );
        assert!(
            dialog
                .show_save_dialog(SaveDialogOptions::default())
                .canceled
        );
    }
}
