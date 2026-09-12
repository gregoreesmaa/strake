//! Drop-in Electron main-process compatibility (issue #21, MVP slice).
//!
//! Existing Electron apps boot on Strake by aliasing `electron` to this
//! shim's TypeScript surface (to come), which binds 1:1 onto the primitives
//! here:
//!
//! * [`App`] — `app` lifecycle state machine (`whenReady`/`quit`/`getPath`,
//!   `ready` / `window-all-closed` / `activate` / `before-quit` events).
//! * [`WindowManager`] / [`BrowserWindowOptions`] — `BrowserWindow` option
//!   mapping plus headless-testable window state (show/hide/close/minimize/
//!   maximize/focus/title), wired to close the [`App`] loop.
//! * [`IpcBus`] — `ipcMain.handle` / `ipcRenderer.invoke` JSON-string
//!   round-trips plus `send`/`on` fan-out (issue: JSON first, zero-copy
//!   later).
//! * [`TOP50`](crate::coverage::TOP50) — the frozen top-50 API surface:
//!   what is native/shimmed in this MVP versus explicitly deferred (with the
//!   blocking issue named per entry).
//!
//! Deliberately headless: real `winit` windows (`strake-shell`), native
//! dialogs (`dialog`/`shell`, needs #12), and the TS bindings ride on top of
//! this core in follow-ups.

mod app;
mod coverage;
mod ipc;
mod message_channel;
mod shell;
mod web_storage;
mod window;

pub use app::{App, AppEventKind, AppPath};
pub use coverage::{ApiEntry, SupportStatus, TOP50};
pub use ipc::{IpcBus, IpcError, ListenerId};
pub use message_channel::{MessageChannel, Port};
pub use shell::{ShellError, ShellWindow};
pub use web_storage::{
    Origin, STORAGE_QUOTA_BYTES, StorageArea, StorageAreaKind, StorageChange, StorageError,
    StoragePartition, WebStorage,
};
pub use window::{BrowserWindow, BrowserWindowOptions, TitleBarStyle, WebContents, WindowManager};
