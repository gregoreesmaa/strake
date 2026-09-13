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
//!   maximize/focus/title, resizable/bounds from issue #90), wired to close
//!   the [`App`] loop.
//! * [`Screen`] / [`Display`] — display enumeration over injected monitor
//!   metrics (issue #96) for `screen.*` and window placement.
//! * [`IpcBus`] — `ipcMain.handle` / `ipcRenderer.invoke` JSON-string
//!   round-trips plus `send`/`on` fan-out (issue: JSON first, zero-copy
//!   later).
//! * [`TOP50`](crate::coverage::TOP50) — the frozen top-50 API surface:
//!   what is native/shimmed in this MVP versus explicitly deferred (with the
//!   blocking issue named per entry).
//! * [`Clipboard`] / [`NotificationCenter`] / [`PowerHub`] / [`SafeStorage`]
//!   — OS bridges (issues #95/#92/#93/#94) fronting injectable backends so
//!   headless CI stays hermetic while the runtime wires the native seat.
//! * [`Dialog`] / [`OsShell`] / [`NativeTheme`] — `dialog`, `shell`, and
//!   `nativeTheme` headless contracts (issue #21 step 2) over the same
//!   backend-trait seam; OS panels/launchers/theme seats bind next (#12).
//!
//! Deliberately headless: real `winit` windows (`strake-shell`), OS-native
//! panels and menus (needs #12), and the TS bindings ride on top of this
//! core in follow-ups.

mod app;
mod clipboard;
mod coverage;
mod dialog;
mod ipc;
mod menu;
mod message_channel;
mod napi;
mod native_theme;
mod notification;
mod os_shell;
mod permissions;
mod power;
mod safe_storage;
mod screen;
mod shell;
mod updater;
mod web_storage;
mod window;

pub use app::{App, AppEventKind, AppPath};
pub use clipboard::{Clipboard, ClipboardBackend, ClipboardError, MemoryClipboard};
pub use coverage::{ApiEntry, SupportStatus, TOP50};
pub use dialog::{
    Dialog, DialogBackend, FileFilter, MemoryDialog, MessageBoxOptions, MessageBoxResult,
    MessageBoxType, OpenDialogOptions, OpenDialogResult, OpenProperty, SaveDialogOptions,
    SaveDialogResult, ScriptedDialog,
};
pub use ipc::{IpcBus, IpcError, ListenerId};
pub use menu::{
    Accelerator, AcceleratorError, MenuError, MenuItemTemplate, MenuItemType, MenuRole,
    MenuTemplate, Modifier, parse_accelerator,
};
pub use message_channel::{MessageChannel, Port};
pub use napi::{
    AddonLoad, AddonRequirements, IMPLEMENTED_SYMBOLS, NapiStatus, REQUIRED_SYMBOLS,
    check_addon_load, missing_symbols,
};
pub use native_theme::{NativeTheme, ThemeListenerId, ThemeSource};
pub use notification::{
    DeliveredNotification, NotificationBackend, NotificationCenter, NotificationRequest,
    RecordingBackend,
};
pub use os_shell::{OsShell, OsShellBackend, OsShellError, RecordingOsShell};
pub use permissions::{Decision, Enforcer, NetScope, PathScope, PermissionManifest, clean_path};
pub use power::{
    PowerEvent, PowerHub, PowerListenerId, PowerMonitor, PowerSaveBlocker, PowerSaveBlockerKind,
};
pub use safe_storage::{KeychainBackend, RecordingKeychain, SafeStorage, SafeStorageError};
pub use screen::{Display, Screen};
pub use shell::{FileOnlyNetProvider, ShellError, ShellWindow};
pub use updater::{Artifact, ManifestError, UpdateManifest, Version};
pub use web_storage::{
    Origin, STORAGE_QUOTA_BYTES, StorageArea, StorageAreaKind, StorageChange, StorageError,
    StoragePartition, WebStorage,
};
pub use window::{
    Bounds, BrowserWindow, BrowserWindowOptions, MainSend, TitleBarStyle, WebContents,
    WebPreferences, WindowManager,
};
