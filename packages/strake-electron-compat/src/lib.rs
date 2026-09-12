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
//! * [`Clipboard`] / [`NotificationCenter`] / [`PowerHub`] / [`SafeStorage`]
//!   — OS bridges (issues #95/#92/#93/#94) fronting injectable backends so
//!   headless CI stays hermetic while the runtime wires the native seat.
//!
//! Deliberately headless: real `winit` windows (`strake-shell`), native
//! dialogs (`dialog`/`shell`, needs #12), and the TS bindings ride on top of
//! this core in follow-ups.

mod app;
mod clipboard;
mod coverage;
mod ipc;
mod notification;
mod power;
mod safe_storage;
mod shell;
mod window;

pub use app::{App, AppEventKind, AppPath};
pub use clipboard::{Clipboard, ClipboardBackend, ClipboardError, MemoryClipboard};
pub use coverage::{ApiEntry, SupportStatus, TOP50};
pub use ipc::{IpcBus, IpcError, ListenerId};
pub use notification::{
    DeliveredNotification, NotificationBackend, NotificationCenter, NotificationRequest,
    RecordingBackend,
};
pub use power::{
    PowerEvent, PowerHub, PowerListenerId, PowerMonitor, PowerSaveBlocker, PowerSaveBlockerKind,
};
pub use safe_storage::{KeychainBackend, RecordingKeychain, SafeStorage, SafeStorageError};
pub use shell::{ShellError, ShellWindow};
pub use window::{BrowserWindow, BrowserWindowOptions, TitleBarStyle, WebContents, WindowManager};
