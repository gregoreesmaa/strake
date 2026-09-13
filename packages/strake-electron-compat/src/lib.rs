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
//!
//! Deliberately headless: real `winit` windows (`strake-shell`), native
//! dialogs (`dialog`/`shell`, needs #12), and the TS bindings ride on top of
//! this core in follow-ups.

mod app;
mod coverage;
mod ipc;
mod screen;
mod shell;
mod window;

pub use app::{App, AppEventKind, AppPath};
pub use coverage::{ApiEntry, SupportStatus, TOP50};
pub use ipc::{IpcBus, IpcError, ListenerId};
pub use screen::{Display, Screen};
pub use shell::{ShellError, ShellWindow};
pub use window::{
    Bounds, BrowserWindow, BrowserWindowOptions, MainSend, TitleBarStyle, WebContents,
    WindowManager,
};
