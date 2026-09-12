//! `require('electron')` for main-process scripts (issue #81, Slice 1) and
//! renderer pages (issue #83, Slice 3).
//!
//! Binds an Electron `main.js` onto [`strake_electron_compat`](https://github.com/gregoreesmaa/strake/issues/21):
//! an [`ElectronHost`] owns the compat core (`App`, `WindowManager`) plus the
//! JS callbacks a main script registers, and [`ScriptDocument::install_electron`](crate::ScriptDocument::install_electron)
//! exposes them as a CommonJS-style `require('electron')` returning
//! `{ app, BrowserWindow, ipcMain }`.
//!
//! Headless scope (Slice 1): no OS window exists yet (Slice 2 binds
//! `BrowserWindow` to `strake-shell`), and renderer `ipcRenderer` invocation
//! is Slice 3. Concretely:
//!
//! * `app.whenReady()` returns a promise resolved by
//!   [`ScriptDocument::mark_electron_ready`](crate::ScriptDocument::mark_electron_ready);
//!   `app.on(event, cb)` stores JS listeners for the five `AppEventKind`
//!   names (any other name is accepted and never fires, matching Electron's
//!   `EventEmitter`);
//! * `new BrowserWindow(options)` creates a compat-core window (`width`,
//!   `height`, `show`, `title` are honoured; the rest is accepted and
//!   ignored); `loadFile`/`loadURL`/`show`/`close` drive it;
//! * `ipcMain.handle`/`on` record the JS handler for observation via
//!   [`ElectronHost::ipc_handler_channels`]. Registration — not invocation —
//!   is the Slice 1 contract: invoking a JS handler needs a JS `Context` at
//!   invoke time, which only the Slice 3 renderer bridge holds. Slice 3 will
//!   pump these registrations through `ipcRenderer.invoke`/`send`.
//! * `clipboard` (`readText`/`writeText`/`clear`, issue #95),
//!   `safeStorage` (`isEncryptionAvailable`/`encryptString`/`decryptString`,
//!   issue #94), `powerMonitor.on` plus `powerSaveBlocker`
//!   (`start`/`stop`/`isStarted`, issue #93) are backed by the compat OS
//!   bridges (memory/recording backends headless); queued power events reach
//!   JS listeners via `dispatch_power_events` on the main document.
//! * Closing the last window fires JS `window-all-closed` listeners and feeds
//!   the count into `App` (Electron's default quit).
//!
//! Renderer scope (Slice 3): [`ScriptDocument::install_electron_renderer`](crate::ScriptDocument::install_electron_renderer)
//! exposes `require('electron').ipcRenderer` (`invoke`/`send`/`on`) plus the
//! `Notification` Web-API shape (`title`/`body`/`icon`/`onclick`, issue #92)
//! to a page document sharing the same [`ElectronHost`]. Renderer calls
//! queue JSON payloads; the embedder runs [`ScriptDocument::pump_ipc`](crate::ScriptDocument::pump_ipc)
//! to invoke main-process handlers and settle renderer promises — the
//! headless analogue of Electron's cross-process IPC round-trip. Payloads
//! follow `JSON.stringify` loosely (functions/`undefined`/symbols vanish,
//! non-finite numbers become `null`); handler errors reject with Electron's
//! `No handler registered for '<channel>'` message for unknown channels.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::{JsArray, JsFunction, JsPromise};
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
};
use strake_electron_compat::{
    App, BrowserWindowOptions, Clipboard, NotificationCenter, NotificationRequest, PowerHub,
    PowerSaveBlockerKind, SafeStorage, WindowManager,
};
// Re-exported for embedders driving the issue #92/#93 probes.
pub use strake_electron_compat::{DeliveredNotification, PowerEvent};

use crate::dom::{dom_ctx, to_rust_string};
use crate::engine::ScriptEngine;

/// JS bootstrap shaping the Electron API over the native primitives below.
///
/// Kept in JS (like the runtime bootstrap) so the API surface stays reviewable
/// without native class plumbing; every state mutation bottoms out in a
/// `__strake_electron_*` primitive backed by the compat core.
const ELECTRON_BOOTSTRAP_JS: &str = r#"
(function () {
    const electron = {};
    electron.app = {
        whenReady() {
            return globalThis.__strake_electron_app_when_ready();
        },
        on(event, listener) {
            globalThis.__strake_electron_app_on(event, listener);
        },
        quit() {
            globalThis.__strake_electron_app_quit();
        },
        isReady() {
            return globalThis.__strake_electron_app_is_ready();
        },
        getName() {
            return globalThis.__strake_electron_app_get_name();
        },
        getVersion() {
            return globalThis.__strake_electron_app_get_version();
        },
    };
    electron.BrowserWindow = class BrowserWindow {
        constructor(options) {
            this.__strakeWindowId = globalThis.__strake_electron_window_create(options || {});
        }
        loadFile(path) {
            globalThis.__strake_electron_window_load_file(this.__strakeWindowId, path);
        }
        loadURL(url) {
            globalThis.__strake_electron_window_load_url(this.__strakeWindowId, url);
        }
        show() {
            globalThis.__strake_electron_window_show(this.__strakeWindowId);
        }
        close() {
            globalThis.__strake_electron_window_close(this.__strakeWindowId);
        }
    };
    electron.ipcMain = {
        handle(channel, handler) {
            globalThis.__strake_electron_ipc_handle(channel, handler);
        },
        on(channel, listener) {
            globalThis.__strake_electron_ipc_on(channel, listener);
        },
    };
    electron.clipboard = {
        readText() {
            return globalThis.__strake_electron_clipboard_read_text();
        },
        writeText(text) {
            globalThis.__strake_electron_clipboard_write_text(text);
        },
        clear() {
            globalThis.__strake_electron_clipboard_clear();
        },
    };
    electron.safeStorage = {
        isEncryptionAvailable() {
            return globalThis.__strake_electron_safe_storage_is_available();
        },
        encryptString(plainText) {
            return globalThis.__strake_electron_safe_storage_encrypt(plainText);
        },
        decryptString(encrypted) {
            return globalThis.__strake_electron_safe_storage_decrypt(encrypted);
        },
    };
    electron.powerMonitor = {
        on(event, listener) {
            globalThis.__strake_electron_power_monitor_on(event, listener);
        },
    };
    electron.powerSaveBlocker = {
        start(type) {
            return globalThis.__strake_electron_power_save_blocker_start(type);
        },
        stop(id) {
            return globalThis.__strake_electron_power_save_blocker_stop(id);
        },
        isStarted(id) {
            return globalThis.__strake_electron_power_save_blocker_is_started(id);
        },
    };
    globalThis.__strake_electron_module = electron;
})();
"#;

/// Mutable Electron main-process state shared between the native primitives
/// (which run inside JS calls) and the [`ElectronHost`] handle held by the
/// embedder. Single-threaded by construction (Boa contexts are `!Send`).
struct ElectronHostState {
    app: App,
    windows: WindowManager,
    /// The assembled `{ app, BrowserWindow, ipcMain }` object, built once at
    /// install so every `require('electron')` returns the identical object
    /// (Node module caching).
    module: Option<JsObject>,
    /// JS `app.on(event, cb)` listeners keyed by event name.
    app_listeners: HashMap<String, Vec<JsObject>>,
    /// Pending `app.whenReady()` resolvers, settled by `mark_electron_ready`.
    when_ready_resolvers: Vec<JsFunction>,
    /// `ipcMain.handle` registrations (channel -> JS handler). Observed, not
    /// invoked, in Slice 1 (see module docs).
    ipc_handlers: HashMap<String, JsObject>,
    /// `ipcMain.on` registrations (channel -> JS listeners).
    ipc_listeners: HashMap<String, Vec<JsObject>>,
    /// Compat window ids in creation order, for embedder observation.
    created_window_ids: Vec<u32>,
    /// The renderer `{ ipcRenderer }` module object (per-context twin of
    /// [`ElectronHostState::module`]; contexts cannot share JS objects).
    renderer_module: Option<JsObject>,
    /// `ipcRenderer.on` registrations (channel -> renderer JS listeners).
    renderer_listeners: HashMap<String, Vec<JsObject>>,
    /// `ipcRenderer.invoke` calls awaiting the main-process pump.
    invoke_queue: VecDeque<PendingInvoke>,
    /// `ipcRenderer.send` broadcasts awaiting the main-process pump.
    send_queue: VecDeque<PendingSend>,
    /// OS clipboard binding (issue #95; memory backend headless).
    clipboard: Clipboard,
    /// Renderer notification hub (issue #92; recording backend headless).
    notifications: NotificationCenter,
    /// `Notification` `onclick` handlers by delivery id (renderer scope).
    notification_clicks: HashMap<u64, JsObject>,
    /// Shared power hub: blocker registry plus the synthetic-probe queue.
    power: PowerHub,
    /// `powerMonitor.on` JS listeners by event name (main scope).
    power_listeners: HashMap<String, Vec<JsObject>>,
    /// OS keychain binding (issue #94; recording backend headless).
    safe_storage: SafeStorage,
}

/// `powerMonitor` event names carried to JS listeners.
fn power_event_name(event: PowerEvent) -> &'static str {
    match event {
        PowerEvent::Suspend => "suspend",
        PowerEvent::Resume => "resume",
        PowerEvent::OnAc => "on-ac",
        PowerEvent::OnBattery => "on-battery",
    }
}

/// One `ipcRenderer.invoke` awaiting [`ScriptDocument::pump_ipc`](crate::ScriptDocument::pump_ipc).
struct PendingInvoke {
    channel: String,
    args: Vec<serde_json::Value>,
    resolve: JsFunction,
    reject: JsFunction,
}

/// One `ipcRenderer.send` awaiting [`ScriptDocument::pump_ipc`](crate::ScriptDocument::pump_ipc).
struct PendingSend {
    channel: String,
    args: Vec<serde_json::Value>,
}

/// Shareable handle to [`ElectronHostState`], stored in the Boa context's
/// host data (mirroring `DomCtx`) so native primitives can reach it.
#[derive(Clone)]
pub(crate) struct SharedElectronHost(Rc<RefCell<ElectronHostState>>);

/// Owns the Electron main-process compat core for one script context.
///
/// Created with [`ElectronHost::new`], installed with
/// [`ScriptDocument::install_electron`](crate::ScriptDocument::install_electron),
/// driven with [`ScriptDocument::mark_electron_ready`](crate::ScriptDocument::mark_electron_ready),
/// and observed through the accessors below.
pub struct ElectronHost {
    shared: SharedElectronHost,
}

impl ElectronHost {
    /// A new host with fresh `App` identity and an empty window manager.
    pub fn new(app_name: &str, app_version: &str) -> Self {
        Self {
            shared: SharedElectronHost(Rc::new(RefCell::new(ElectronHostState {
                app: App::new(app_name, app_version),
                windows: WindowManager::new(),
                module: None,
                app_listeners: HashMap::new(),
                when_ready_resolvers: Vec::new(),
                ipc_handlers: HashMap::new(),
                ipc_listeners: HashMap::new(),
                created_window_ids: Vec::new(),
                renderer_module: None,
                renderer_listeners: HashMap::new(),
                invoke_queue: VecDeque::new(),
                send_queue: VecDeque::new(),
                clipboard: Clipboard::default(),
                notifications: NotificationCenter::recording(),
                notification_clicks: HashMap::new(),
                power: PowerHub::new(),
                power_listeners: HashMap::new(),
                safe_storage: SafeStorage::recording(),
            }))),
        }
    }

    pub(crate) fn shared(&self) -> SharedElectronHost {
        self.shared.clone()
    }

    /// Whether `mark_electron_ready` has run (`app.isReady`).
    pub fn is_ready(&self) -> bool {
        self.shared.0.borrow().app.is_ready()
    }

    /// Whether `app.quit()` ran.
    pub fn is_quit(&self) -> bool {
        self.shared.0.borrow().app.is_quit()
    }

    /// Live compat windows.
    pub fn window_count(&self) -> usize {
        self.shared.0.borrow().windows.window_count()
    }

    /// Compat window ids in creation order.
    pub fn created_window_ids(&self) -> Vec<u32> {
        self.shared.0.borrow().created_window_ids.clone()
    }

    /// Pending navigation target of a window (`loadFile`/`loadURL`).
    pub fn window_pending_url(&self, id: u32) -> Option<String> {
        self.shared
            .0
            .borrow()
            .windows
            .get(id)
            .and_then(|win| win.web_contents().pending_url())
            .map(str::to_string)
    }

    /// Channels with an `ipcMain.handle` registration, sorted.
    pub fn ipc_handler_channels(&self) -> Vec<String> {
        let mut channels: Vec<String> = self
            .shared
            .0
            .borrow()
            .ipc_handlers
            .keys()
            .cloned()
            .collect();
        channels.sort();
        channels
    }

    /// Queued `invoke` + `send` calls awaiting
    /// [`ScriptDocument::pump_ipc`](crate::ScriptDocument::pump_ipc).
    pub fn pending_ipc_count(&self) -> usize {
        let state = self.shared.0.borrow();
        state.invoke_queue.len() + state.send_queue.len()
    }

    /// Recorded notification deliveries, in order (issue #92 test observation).
    pub fn notification_delivered(&self) -> Vec<DeliveredNotification> {
        self.shared.0.borrow().notifications.delivered()
    }

    /// Queue a synthetic (or shell-sourced) power event for JS dispatch
    /// (issue #93 probe). Delivered by
    /// [`ScriptDocument::dispatch_power_events`](crate::ScriptDocument::dispatch_power_events).
    pub fn inject_power_event(&self, event: PowerEvent) {
        self.shared.0.borrow().power.inject_for_dispatch(event);
    }

    /// Channels with at least one `ipcMain.on` listener, sorted.
    pub fn ipc_listener_channels(&self) -> Vec<String> {
        let mut channels: Vec<String> = self
            .shared
            .0
            .borrow()
            .ipc_listeners
            .keys()
            .cloned()
            .collect();
        channels.sort();
        channels
    }
}

/// Fetch the host state from the Boa context's host data.
fn electron_state(context: &mut Context) -> JsResult<SharedElectronHost> {
    context
        .get_data::<SharedElectronHost>()
        .cloned()
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Electron host not installed (use ScriptDocument::install_electron)")
                .into()
        })
}

/// Record a failure to invoke an Electron JS callback into the document's
/// error sink (mirrors `report_js_error` in `runtime.rs`).
fn record_callback_error(context: &mut Context, what: &str, error: &JsError) {
    let message = format!("Uncaught JS error in Electron {what}: {error}");
    if let Ok(ctx) = dom_ctx(context) {
        ctx.state.borrow_mut().record_error(message);
    }
}

/// Call JS callbacks with `undefined` as `this`, recording (not propagating)
/// each failure so one throwing listener cannot silence its siblings.
fn call_js_listeners(context: &mut Context, what: &str, listeners: Vec<JsObject>) {
    for listener in listeners {
        if let Err(error) = listener.call(&JsValue::undefined(), &[], context) {
            record_callback_error(context, what, &error);
        }
    }
}

/// Fire the JS listeners registered for an `app` event name.
fn fire_app_event(context: &mut Context, event: &str) {
    let listeners = electron_state(context)
        .map(|shared| {
            shared
                .0
                .borrow_mut()
                .app_listeners
                .remove(event)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    // `ready` (like `before-quit`/`will-quit`, which the compat `App` emits at
    // most once) fires once; `window-all-closed`/`activate` recur across
    // window sets, so those listeners survive firing.
    if event == "window-all-closed" || event == "activate" {
        if let Ok(shared) = electron_state(context) {
            shared
                .0
                .borrow_mut()
                .app_listeners
                .entry(event.to_string())
                .or_default()
                .extend(listeners.iter().cloned());
        }
    }
    call_js_listeners(context, &format!("app '{event}' listener"), listeners);
}

fn require_string_arg(args: &[JsValue], index: usize, what: &str) -> JsResult<String> {
    args.get(index)
        .and_then(|value| value.as_string())
        .map(|s| s.to_std_string_escaped())
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message(format!("{what} requires a string argument"))
                .into()
        })
}

fn require_callable_arg(args: &[JsValue], index: usize, what: &str) -> JsResult<JsObject> {
    args.get(index)
        .and_then(|value| value.as_object())
        .filter(|obj| obj.is_callable())
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message(format!("{what} requires a function argument"))
                .into()
        })
}

fn window_id_arg(args: &[JsValue], context: &mut Context) -> JsResult<u32> {
    let Some(first) = args.first() else {
        return Err(JsNativeError::typ()
            .with_message("window id requires a numeric argument")
            .into());
    };
    let id = first.to_number(context)?;
    if id.is_finite() && id >= 0.0 {
        Ok(id as u32)
    } else {
        Err(JsNativeError::typ()
            .with_message("window id requires a numeric argument")
            .into())
    }
}

/// `require(specifier)`: only `'electron'` resolves; anything else throws
/// Node's "Cannot find module" error.
fn e_require(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let specifier = require_string_arg(args, 0, "require")?;
    if specifier != "electron" {
        return Err(JsError::from(
            JsNativeError::error().with_message(format!("Cannot find module '{specifier}'")),
        ));
    }
    let shared = electron_state(context)?;
    shared
        .0
        .borrow()
        .module
        .clone()
        .map(JsValue::from)
        .ok_or_else(|| {
            JsNativeError::error()
                .with_message("Electron module not initialised")
                .into()
        })
}

/// `app.whenReady()`: an already-resolved promise once ready, otherwise a
/// pending promise settled by `mark_electron_ready`.
fn e_app_when_ready(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let shared = electron_state(context)?;
    if shared.0.borrow().app.is_ready() {
        return Ok(JsValue::from(JsPromise::resolve(
            JsValue::undefined(),
            context,
        )?));
    }
    let (promise, resolvers) = JsPromise::new_pending(context);
    shared
        .0
        .borrow_mut()
        .when_ready_resolvers
        .push(resolvers.resolve);
    Ok(JsValue::from(promise))
}

/// `app.on(event, listener)`: a late `ready` registration fires on the next
/// microtask checkpoint (via an already-resolved promise); other events queue
/// until their transition fires.
fn e_app_on(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let event = require_string_arg(args, 0, "app.on")?;
    let listener = require_callable_arg(args, 1, "app.on")?;
    let shared = electron_state(context)?;
    if event == "ready" && shared.0.borrow().app.is_ready() {
        if let Err(error) = listener.call(&JsValue::undefined(), &[], context) {
            record_callback_error(context, "app 'ready' listener", &error);
        }
        return Ok(JsValue::undefined());
    }
    shared
        .0
        .borrow_mut()
        .app_listeners
        .entry(event)
        .or_default()
        .push(listener);
    Ok(JsValue::undefined())
}

/// `app.quit()`: fire JS `before-quit`/`will-quit` listeners, then run the
/// compat shutdown (which emits the Rust-side events in the same order).
fn e_app_quit(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    fire_app_event(context, "before-quit");
    fire_app_event(context, "will-quit");
    if let Ok(shared) = electron_state(context) {
        shared.0.borrow_mut().app.quit();
    }
    Ok(JsValue::undefined())
}

fn e_app_is_ready(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(
        electron_state(context)?.0.borrow().app.is_ready(),
    ))
}

fn e_app_get_name(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = electron_state(context)?.0.borrow().app.name().to_string();
    Ok(JsValue::from(js_string!(name.as_str())))
}

fn e_app_get_version(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let version = electron_state(context)?
        .0
        .borrow()
        .app
        .version()
        .to_string();
    Ok(JsValue::from(js_string!(version.as_str())))
}

fn options_u32(
    options: &JsObject,
    name: &str,
    default: u32,
    context: &mut Context,
) -> JsResult<u32> {
    let value = options.get(JsString::from(name), context)?;
    if value.is_undefined() || value.is_null() {
        return Ok(default);
    }
    let number = value.to_number(context)?;
    if number.is_finite() && number >= 0.0 {
        Ok(number as u32)
    } else {
        Ok(default)
    }
}

fn options_bool(
    options: &JsObject,
    name: &str,
    default: bool,
    context: &mut Context,
) -> JsResult<bool> {
    let value = options.get(JsString::from(name), context)?;
    if value.is_undefined() || value.is_null() {
        return Ok(default);
    }
    Ok(value.to_boolean())
}

/// `new BrowserWindow(options)`: create the compat-core window. Only
/// `width`/`height`/`show`/`title` shape Slice 1 behavior; every other
/// Electron option is accepted and ignored (Slice 2 binds the rest to
/// `strake-shell`).
fn e_window_create(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let shared = electron_state(context)?;
    let mut options = BrowserWindowOptions::default();
    if let Some(obj) = args.first().and_then(|value| value.as_object()) {
        options.width = options_u32(&obj, "width", 800, context)?;
        options.height = options_u32(&obj, "height", 600, context)?;
        options.show = options_bool(&obj, "show", true, context)?;
        let title = obj.get(js_string!("title"), context)?;
        if !title.is_undefined() && !title.is_null() {
            options.title = to_rust_string(&title, context)?;
        }
    }
    let mut state = shared.0.borrow_mut();
    let id = state.windows.create(options);
    state.created_window_ids.push(id);
    Ok(JsValue::from(id as f64))
}

/// Look up a live window or throw Electron's "Object has been destroyed".
fn window_mut(
    state: &mut ElectronHostState,
    id: u32,
) -> JsResult<&mut strake_electron_compat::BrowserWindow> {
    state.windows.get_mut(id).ok_or_else(|| {
        JsError::from(JsNativeError::error().with_message("Object has been destroyed"))
    })
}

fn e_window_load_file(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let path = require_string_arg(args, 1, "win.loadFile")?;
    let shared = electron_state(context)?;
    let mut state = shared.0.borrow_mut();
    window_mut(&mut state, id)?
        .web_contents_mut()
        .load_file(&path);
    Ok(JsValue::undefined())
}

fn e_window_load_url(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let url = require_string_arg(args, 1, "win.loadURL")?;
    let shared = electron_state(context)?;
    let mut state = shared.0.borrow_mut();
    window_mut(&mut state, id)?
        .web_contents_mut()
        .load_url(&url);
    Ok(JsValue::undefined())
}

fn e_window_show(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let shared = electron_state(context)?;
    shared.0.borrow_mut().windows.show(id);
    Ok(JsValue::undefined())
}

/// `win.close()`: destroy the window; the last close fires JS
/// `window-all-closed` listeners and runs the compat shutdown flow
/// (Electron's default quit).
fn e_window_close(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let shared = electron_state(context)?;
    let last_closed = {
        let mut state = shared.0.borrow_mut();
        state.windows.close(id);
        state.windows.window_count() == 0
    };
    if last_closed {
        shared.0.borrow_mut().app.note_window_closed(0);
        fire_app_event(context, "window-all-closed");
    }
    Ok(JsValue::undefined())
}

/// `ipcMain.handle(channel, handler)`: record the JS handler. Like Electron,
/// a second handler for the same channel throws.
fn e_ipc_handle(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = require_string_arg(args, 0, "ipcMain.handle")?;
    let handler = require_callable_arg(args, 1, "ipcMain.handle")?;
    let shared = electron_state(context)?;
    let mut state = shared.0.borrow_mut();
    if state.ipc_handlers.contains_key(&channel) {
        return Err(JsError::from(JsNativeError::error().with_message(format!(
            "Attempted to register a second handler for '{channel}'"
        ))));
    }
    state.ipc_handlers.insert(channel, handler);
    Ok(JsValue::undefined())
}

/// Optional string argument (`undefined`/`null`/missing map to `None`).
fn optional_string_arg(
    args: &[JsValue],
    index: usize,
    context: &mut Context,
) -> JsResult<Option<String>> {
    match args.get(index) {
        None => Ok(None),
        Some(value) if value.is_undefined() || value.is_null() => Ok(None),
        Some(value) => to_rust_string(value, context).map(Some),
    }
}

/// Numeric id argument with a caller-named error.
fn numeric_id_arg(args: &[JsValue], context: &mut Context, what: &str) -> JsResult<u64> {
    let Some(first) = args.first() else {
        return Err(JsError::from(
            JsNativeError::typ().with_message(format!("{what} requires a numeric id")),
        ));
    };
    let id = first.to_number(context)?;
    if id.is_finite() && id >= 0.0 {
        Ok(id as u64)
    } else {
        Err(JsError::from(
            JsNativeError::typ().with_message(format!("{what} requires a numeric id")),
        ))
    }
}

/// `clipboard.readText()` (issue #95). Unavailable/empty maps to `""`.
fn e_clipboard_read_text(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = electron_state(context)?
        .0
        .borrow()
        .clipboard
        .read_text()
        .unwrap_or_default();
    Ok(JsValue::from(js_string!(text.as_str())))
}

/// `clipboard.writeText(text)` (issue #95).
fn e_clipboard_write_text(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let text = require_string_arg(args, 0, "clipboard.writeText")?;
    electron_state(context)?
        .0
        .borrow()
        .clipboard
        .write_text(&text)
        .map_err(|error| {
            JsError::from(
                JsNativeError::error().with_message(format!("clipboard.writeText failed: {error}")),
            )
        })?;
    Ok(JsValue::undefined())
}

/// `clipboard.clear()` (issue #95).
fn e_clipboard_clear(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    electron_state(context)?
        .0
        .borrow()
        .clipboard
        .clear()
        .map_err(|error| {
            JsError::from(
                JsNativeError::error().with_message(format!("clipboard.clear failed: {error}")),
            )
        })?;
    Ok(JsValue::undefined())
}

/// `safeStorage.isEncryptionAvailable()` (issue #94).
fn e_safe_storage_is_available(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(
        electron_state(context)?
            .0
            .borrow()
            .safe_storage
            .is_encryption_available(),
    ))
}

/// `safeStorage.encryptString(plain)` → base64 ciphertext (issue #94).
fn e_safe_storage_encrypt(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let plain = require_string_arg(args, 0, "safeStorage.encryptString")?;
    let sealed = electron_state(context)?
        .0
        .borrow()
        .safe_storage
        .encrypt_string(&plain)
        .map_err(|error| {
            JsError::from(
                JsNativeError::error()
                    .with_message(format!("safeStorage.encryptString failed: {error}")),
            )
        })?;
    Ok(JsValue::from(js_string!(sealed.as_str())))
}

/// `safeStorage.decryptString(base64)` → plaintext (issue #94).
fn e_safe_storage_decrypt(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let sealed = require_string_arg(args, 0, "safeStorage.decryptString")?;
    let plain = electron_state(context)?
        .0
        .borrow()
        .safe_storage
        .decrypt_string(&sealed)
        .map_err(|error| {
            JsError::from(
                JsNativeError::error()
                    .with_message(format!("safeStorage.decryptString failed: {error}")),
            )
        })?;
    Ok(JsValue::from(js_string!(plain.as_str())))
}

/// `powerMonitor.on(event, listener)` (issue #93): record a JS listener.
/// Unknown event names are accepted and never fire, matching `app.on`.
fn e_power_monitor_on(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let event = require_string_arg(args, 0, "powerMonitor.on")?;
    let listener = require_callable_arg(args, 1, "powerMonitor.on")?;
    electron_state(context)?
        .0
        .borrow_mut()
        .power_listeners
        .entry(event)
        .or_default()
        .push(listener);
    Ok(JsValue::undefined())
}

/// `powerSaveBlocker.start(type)` → hold id (issue #93).
fn e_power_save_blocker_start(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let kind = require_string_arg(args, 0, "powerSaveBlocker.start")?;
    let kind = match kind.as_str() {
        "prevent-app-suspension" => PowerSaveBlockerKind::PreventAppSuspension,
        "prevent-display-sleep" => PowerSaveBlockerKind::PreventDisplaySleep,
        other => {
            return Err(JsError::from(JsNativeError::typ().with_message(format!(
                "powerSaveBlocker.start: unknown type '{other}'"
            ))));
        }
    };
    let shared = electron_state(context)?;
    let id = shared
        .0
        .borrow()
        .power
        .with_blocker(|blocker| blocker.start(kind));
    Ok(JsValue::from(id as f64))
}

/// `powerSaveBlocker.stop(id)` (issue #93).
fn e_power_save_blocker_stop(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = numeric_id_arg(args, context, "powerSaveBlocker.stop")?;
    let shared = electron_state(context)?;
    Ok(JsValue::from(
        shared
            .0
            .borrow()
            .power
            .with_blocker(|blocker| blocker.stop(id)),
    ))
}

/// `powerSaveBlocker.isStarted(id)` (issue #93).
fn e_power_save_blocker_is_started(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = numeric_id_arg(args, context, "powerSaveBlocker.isStarted")?;
    let shared = electron_state(context)?;
    Ok(JsValue::from(
        shared
            .0
            .borrow()
            .power
            .with_blocker(|blocker| blocker.is_started(id)),
    ))
}

/// `ipcMain.on(channel, listener)`: record a broadcast listener.
fn e_ipc_on(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = require_string_arg(args, 0, "ipcMain.on")?;
    let listener = require_callable_arg(args, 1, "ipcMain.on")?;
    electron_state(context)?
        .0
        .borrow_mut()
        .ipc_listeners
        .entry(channel)
        .or_default()
        .push(listener);
    Ok(JsValue::undefined())
}

fn register_primitive(
    context: &mut Context,
    name: &str,
    length: usize,
    body: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
) {
    context
        .register_global_callable(
            JsString::from(name),
            length,
            NativeFunction::from_fn_ptr(body),
        )
        .expect("failed to register Electron primitive");
}

impl crate::runtime::ScriptRuntime {
    /// Install the Electron host: primitives, the `require` global, and the
    /// assembled module object (idempotent: reinstalling replaces the host).
    pub(crate) fn install_electron_host(&mut self, shared: &SharedElectronHost) {
        register_primitive(&mut self.context, "require", 1, e_require);
        register_primitive(
            &mut self.context,
            "__strake_electron_app_when_ready",
            0,
            e_app_when_ready,
        );
        register_primitive(&mut self.context, "__strake_electron_app_on", 2, e_app_on);
        register_primitive(
            &mut self.context,
            "__strake_electron_app_quit",
            0,
            e_app_quit,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_is_ready",
            0,
            e_app_is_ready,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_get_name",
            0,
            e_app_get_name,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_get_version",
            0,
            e_app_get_version,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_create",
            1,
            e_window_create,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_load_file",
            2,
            e_window_load_file,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_load_url",
            2,
            e_window_load_url,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_show",
            1,
            e_window_show,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_close",
            1,
            e_window_close,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_ipc_handle",
            2,
            e_ipc_handle,
        );
        register_primitive(&mut self.context, "__strake_electron_ipc_on", 2, e_ipc_on);
        register_primitive(
            &mut self.context,
            "__strake_electron_clipboard_read_text",
            0,
            e_clipboard_read_text,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_clipboard_write_text",
            1,
            e_clipboard_write_text,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_clipboard_clear",
            0,
            e_clipboard_clear,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_safe_storage_is_available",
            0,
            e_safe_storage_is_available,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_safe_storage_encrypt",
            1,
            e_safe_storage_encrypt,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_safe_storage_decrypt",
            1,
            e_safe_storage_decrypt,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_power_monitor_on",
            2,
            e_power_monitor_on,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_power_save_blocker_start",
            1,
            e_power_save_blocker_start,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_power_save_blocker_stop",
            1,
            e_power_save_blocker_stop,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_power_save_blocker_is_started",
            1,
            e_power_save_blocker_is_started,
        );

        self.context.insert_data(shared.clone());
        self.eval(ELECTRON_BOOTSTRAP_JS, "<strake-electron-bootstrap>");

        // Pin the assembled module so `require` returns the identical object.
        let module = self
            .context
            .global_object()
            .get(js_string!("__strake_electron_module"), &mut self.context)
            .ok()
            .and_then(|value| value.as_object());
        shared.0.borrow_mut().module = module;
    }

    /// Runtime initialisation finished: mark the compat `App` ready, fire JS
    /// `ready` listeners, settle `whenReady()` promises, then drain the
    /// microtask queue so `.then` continuations (window creation) run. Safe
    /// to call repeatedly; later calls only re-drain microtasks.
    pub(crate) fn mark_electron_ready(&mut self) {
        let Some(shared) = self.context.get_data::<SharedElectronHost>().cloned() else {
            return;
        };
        let (ready_listeners, resolvers) = {
            let mut state = shared.0.borrow_mut();
            state.app.mark_ready();
            let listeners = state.app_listeners.remove("ready").unwrap_or_default();
            let resolvers = std::mem::take(&mut state.when_ready_resolvers);
            (listeners, resolvers)
        };
        call_js_listeners(&mut self.context, "app 'ready' listener", ready_listeners);
        for resolve in resolvers {
            if let Err(error) = resolve.call(&JsValue::undefined(), &[], &mut self.context) {
                record_callback_error(&mut self.context, "app.whenReady() resolver", &error);
            }
        }
        self.run_jobs("electron ready");
    }
}

// === Slice 3: renderer `ipcRenderer` (issue #83) ===

/// JS bootstrap for renderer pages: `ipcRenderer` over the native queueing
/// primitives. The module intentionally exposes nothing else: renderers get
/// no `app`, `BrowserWindow`, or `ipcMain` (matching Electron without
/// `nodeIntegration`).
const RENDERER_BOOTSTRAP_JS: &str = r#"
(function () {
    const ipcRenderer = {
        invoke(channel, ...args) {
            return globalThis.__strake_ipc_renderer_invoke(channel, ...args);
        },
        send(channel, ...args) {
            globalThis.__strake_ipc_renderer_send(channel, ...args);
        },
        on(channel, listener) {
            globalThis.__strake_ipc_renderer_on(channel, listener);
        },
    };
    globalThis.__strake_electron_renderer_module = { ipcRenderer };
    // Web Notifications shape bound to the compat NotificationCenter
    // (issue #92): construction records the delivery, `onclick` assignment
    // registers the click handler the embedder dispatches.
    globalThis.Notification = class Notification {
        constructor(title, options) {
            const opts = options || {};
            this.__strakeNotificationId = globalThis.__strake_notification_show(
                title === undefined || title === null ? "" : String(title),
                opts.body === undefined || opts.body === null ? undefined : String(opts.body),
                opts.icon === undefined || opts.icon === null ? undefined : String(opts.icon),
            );
            this.__strakeOnclick = null;
        }
        get onclick() {
            return this.__strakeOnclick;
        }
        set onclick(handler) {
            // WebIDL EventHandler conversion: only callables are kept, anything
            // else normalizes to null without throwing. The native is called
            // first so the getter and the native map can never disagree.
            const normalized = (typeof handler === "function") ? handler : null;
            globalThis.__strake_notification_onclick(this.__strakeNotificationId, normalized);
            this.__strakeOnclick = normalized;
        }
        close() {
        }
    };
})();
"#;

/// Marshal a JS value into JSON for the trip across the main/renderer context
/// boundary. Follows `JSON.stringify` loosely (see module docs).
fn js_to_json(value: &JsValue, context: &mut Context) -> JsResult<serde_json::Value> {
    if value.is_null() || value.is_undefined() {
        return Ok(serde_json::Value::Null);
    }
    if let Some(flag) = value.as_boolean() {
        return Ok(serde_json::Value::Bool(flag));
    }
    if let Some(number) = value.as_number() {
        return Ok(serde_json::Number::from_f64(number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null));
    }
    if let Some(text) = value.as_string() {
        return Ok(serde_json::Value::String(text.to_std_string_escaped()));
    }
    if let Some(obj) = value.as_object() {
        if obj.is_callable() {
            return Ok(serde_json::Value::Null);
        }
        if obj.is_array() {
            let len = obj
                .get(JsString::from("length"), context)?
                .to_number(context)
                .unwrap_or(0.0);
            let len = if len.is_finite() && len > 0.0 {
                (len as usize).min(1 << 20)
            } else {
                0
            };
            let mut items = Vec::with_capacity(len.min(64));
            for index in 0..len {
                items.push(js_to_json(&obj.get(index as u32, context)?, context)?);
            }
            return Ok(serde_json::Value::Array(items));
        }
        let mut map = serde_json::Map::new();
        for key in obj.own_property_keys(context)? {
            let name = match &key {
                boa_engine::property::PropertyKey::String(text) => text.to_std_string_escaped(),
                boa_engine::property::PropertyKey::Index(index) => index.get().to_string(),
                boa_engine::property::PropertyKey::Symbol(_) => continue,
            };
            let prop = obj.get(key, context)?;
            if prop.is_undefined() || prop.is_callable() || prop.as_symbol().is_some() {
                continue;
            }
            map.insert(name, js_to_json(&prop, context)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    Ok(serde_json::Value::Null)
}

/// Unmarshal a JSON payload back into a fresh value in `context`.
fn json_to_js(value: &serde_json::Value, context: &mut Context) -> JsResult<JsValue> {
    match value {
        serde_json::Value::Null => Ok(JsValue::null()),
        serde_json::Value::Bool(flag) => Ok(JsValue::from(*flag)),
        serde_json::Value::Number(number) => Ok(number
            .as_f64()
            .map(JsValue::from)
            .unwrap_or(JsValue::null())),
        serde_json::Value::String(text) => Ok(JsValue::from(JsString::from(text.as_str()))),
        serde_json::Value::Array(items) => {
            let mut elements = Vec::with_capacity(items.len());
            for item in items {
                elements.push(json_to_js(item, context)?);
            }
            Ok(JsValue::from(JsArray::from_iter(elements, context)))
        }
        serde_json::Value::Object(map) => {
            // Build child values first: `ObjectInitializer` holds its
            // `&mut Context` borrow, which the recursion also needs.
            let mut props = Vec::with_capacity(map.len());
            for (name, item) in map {
                props.push((JsString::from(name.as_str()), json_to_js(item, context)?));
            }
            let mut init = ObjectInitializer::new(context);
            for (name, value) in props {
                init.property(name, value, Attribute::all());
            }
            Ok(JsValue::from(init.build()))
        }
    }
}

/// Collect the trailing call arguments as JSON payloads.
fn json_args(args: &[JsValue], context: &mut Context) -> JsResult<Vec<serde_json::Value>> {
    args.iter().map(|arg| js_to_json(arg, context)).collect()
}

/// `require('electron')` inside a renderer page: only the renderer module
/// resolves (the main-process module object belongs to another context and
/// must never leak across).
fn e_require_renderer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let specifier = require_string_arg(args, 0, "require")?;
    if specifier != "electron" {
        return Err(JsError::from(
            JsNativeError::error().with_message(format!("Cannot find module '{specifier}'")),
        ));
    }
    let shared = electron_state(context)?;
    shared
        .0
        .borrow()
        .renderer_module
        .clone()
        .map(JsValue::from)
        .ok_or_else(|| {
            JsNativeError::error()
                .with_message("Electron renderer module not initialised")
                .into()
        })
}

/// `ipcRenderer.invoke(channel, ...args)`: queue the call and return the
/// pending promise, settled by [`ScriptDocument::pump_ipc`](crate::ScriptDocument::pump_ipc).
fn e_ipc_renderer_invoke(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let channel = require_string_arg(args, 0, "ipcRenderer.invoke")?;
    let payloads = json_args(args.get(1..).unwrap_or(&[]), context)?;
    let (promise, resolvers) = JsPromise::new_pending(context);
    electron_state(context)?
        .0
        .borrow_mut()
        .invoke_queue
        .push_back(PendingInvoke {
            channel,
            args: payloads,
            resolve: resolvers.resolve,
            reject: resolvers.reject,
        });
    Ok(JsValue::from(promise))
}

/// `ipcRenderer.send(channel, ...args)`: queue a broadcast for the pump.
fn e_ipc_renderer_send(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = require_string_arg(args, 0, "ipcRenderer.send")?;
    let payloads = json_args(args.get(1..).unwrap_or(&[]), context)?;
    electron_state(context)?
        .0
        .borrow_mut()
        .send_queue
        .push_back(PendingSend {
            channel,
            args: payloads,
        });
    Ok(JsValue::undefined())
}

/// `new Notification(title, { body, icon })` (issue #92): record the
/// delivery in the shared center, returning its id for click dispatch.
fn e_notification_show(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let title = optional_string_arg(args, 0, context)?.unwrap_or_default();
    let body = optional_string_arg(args, 1, context)?;
    let icon = optional_string_arg(args, 2, context)?;
    let shared = electron_state(context)?;
    let id = shared
        .0
        .borrow()
        .notifications
        .notify(NotificationRequest { title, body, icon });
    Ok(JsValue::from(id as f64))
}

/// `notification.onclick = handler` (issue #92): register (or, with
/// `null`/`undefined`, clear) the click handler for a delivery id.
fn e_notification_onclick(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = numeric_id_arg(args, context, "Notification.onclick")?;
    let shared = electron_state(context)?;
    match args.get(1) {
        None => Ok(JsValue::undefined()),
        Some(value) if value.is_undefined() || value.is_null() => {
            shared.0.borrow_mut().notification_clicks.remove(&id);
            Ok(JsValue::undefined())
        }
        Some(value) => {
            let handler = value
                .as_object()
                .filter(|obj| obj.is_callable())
                .ok_or_else(|| {
                    JsError::from(
                        JsNativeError::typ()
                            .with_message("Notification.onclick requires a function"),
                    )
                })?;
            shared
                .0
                .borrow_mut()
                .notification_clicks
                .insert(id, handler);
            Ok(JsValue::undefined())
        }
    }
}

/// `ipcRenderer.on(channel, listener)`: register a renderer broadcast
/// listener (invoked by the Slice-3 pump for main-originated sends once a
/// `webContents.send` binding exists; recorded today for symmetry).
fn e_ipc_renderer_on(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = require_string_arg(args, 0, "ipcRenderer.on")?;
    let listener = require_callable_arg(args, 1, "ipcRenderer.on")?;
    electron_state(context)?
        .0
        .borrow_mut()
        .renderer_listeners
        .entry(channel)
        .or_default()
        .push(listener);
    Ok(JsValue::undefined())
}

/// The message a renderer `.catch` should observe for a main-process handler
/// failure: the thrown `Error`'s `message` when it is a JS error, else the
/// full display string (matching Electron's `err.message` passthrough).
fn js_error_message(error: &JsError, context: &mut Context) -> String {
    error
        .try_native(context)
        .map(|native| native.message().to_string())
        .unwrap_or_else(|_| error.to_string())
}

/// Reject an invoke promise with an `Error` carrying `message`.
fn reject_with_message(
    renderer: &mut crate::runtime::ScriptRuntime,
    reject: &JsFunction,
    message: &str,
) {
    let error = JsError::from(JsNativeError::error().with_message(message.to_string()))
        .into_opaque(&mut renderer.context)
        .unwrap_or_else(|_| JsValue::from(JsString::from(message)));
    if let Err(error) = reject.call(&JsValue::undefined(), &[error], &mut renderer.context) {
        record_callback_error(&mut renderer.context, "ipcRenderer.invoke reject", &error);
    }
}

impl crate::runtime::ScriptRuntime {
    /// Install the renderer host: `require('electron').ipcRenderer` backed by
    /// the shared host queues. The same [`ElectronHost`] may back one
    /// main-process document and any number of renderer pages.
    pub(crate) fn install_electron_renderer_host(&mut self, shared: &SharedElectronHost) {
        register_primitive(&mut self.context, "require", 1, e_require_renderer);
        register_primitive(
            &mut self.context,
            "__strake_ipc_renderer_invoke",
            1,
            e_ipc_renderer_invoke,
        );
        register_primitive(
            &mut self.context,
            "__strake_ipc_renderer_send",
            1,
            e_ipc_renderer_send,
        );
        register_primitive(
            &mut self.context,
            "__strake_ipc_renderer_on",
            2,
            e_ipc_renderer_on,
        );
        register_primitive(
            &mut self.context,
            "__strake_notification_show",
            3,
            e_notification_show,
        );
        register_primitive(
            &mut self.context,
            "__strake_notification_onclick",
            2,
            e_notification_onclick,
        );

        self.context.insert_data(shared.clone());
        self.eval(
            RENDERER_BOOTSTRAP_JS,
            "<strake-electron-renderer-bootstrap>",
        );

        let module = self
            .context
            .global_object()
            .get(
                js_string!("__strake_electron_renderer_module"),
                &mut self.context,
            )
            .ok()
            .and_then(|value| value.as_object());
        shared.0.borrow_mut().renderer_module = module;
    }

    /// Pump queued renderer IPC through main-process handlers (issue #83):
    /// each `invoke` runs its `ipcMain.handle` callback in this (main)
    /// context and settles the renderer promise; each `send` fans out to
    /// `ipcMain.on` listeners. Returns the number of pumped calls. All shared
    /// borrows are statement-scoped takes, so re-entrant JS (handlers calling
    /// back into Electron) cannot trip the `RefCell`s.
    pub(crate) fn pump_ipc_to(&mut self, renderer: &mut crate::runtime::ScriptRuntime) -> usize {
        let Some(shared) = self.context.get_data::<SharedElectronHost>().cloned() else {
            return 0;
        };
        let (invokes, sends) = {
            let mut state = shared.0.borrow_mut();
            let invokes: Vec<PendingInvoke> = state.invoke_queue.drain(..).collect();
            let sends: Vec<PendingSend> = state.send_queue.drain(..).collect();
            (invokes, sends)
        };
        let mut pumped = 0;

        for item in invokes {
            pumped += 1;
            let handler = shared.0.borrow().ipc_handlers.get(&item.channel).cloned();
            let Some(handler) = handler else {
                reject_with_message(
                    renderer,
                    &item.reject,
                    &format!("No handler registered for '{}'", item.channel),
                );
                renderer.run_jobs("ipcRenderer.invoke rejection");
                continue;
            };
            // Electron invokes handlers as `(event, ...args)`; the Slice 3
            // event is a minimal stub object (full `sender`/`frameId` shape is
            // a later slice).
            let mut call_args = Vec::with_capacity(item.args.len() + 1);
            call_args.push(JsValue::from(
                ObjectInitializer::new(&mut self.context).build(),
            ));
            for arg in &item.args {
                match json_to_js(arg, &mut self.context) {
                    Ok(value) => call_args.push(value),
                    Err(error) => {
                        record_callback_error(
                            &mut self.context,
                            "ipcRenderer.invoke argument",
                            &error,
                        );
                        call_args.push(JsValue::null());
                    }
                }
            }
            match handler.call(&JsValue::undefined(), &call_args, &mut self.context) {
                Ok(returned) => match js_to_json(&returned, &mut self.context) {
                    Ok(payload) => match json_to_js(&payload, &mut renderer.context) {
                        Ok(value) => {
                            if let Err(error) = item.resolve.call(
                                &JsValue::undefined(),
                                &[value],
                                &mut renderer.context,
                            ) {
                                record_callback_error(
                                    &mut renderer.context,
                                    "ipcRenderer.invoke resolve",
                                    &error,
                                );
                            }
                        }
                        Err(error) => {
                            record_callback_error(
                                &mut renderer.context,
                                "ipcRenderer.invoke result",
                                &error,
                            );
                            reject_with_message(
                                renderer,
                                &item.reject,
                                "failed to marshal handler result",
                            );
                        }
                    },
                    Err(error) => {
                        record_callback_error(
                            &mut self.context,
                            "ipcRenderer.invoke result",
                            &error,
                        );
                        reject_with_message(
                            renderer,
                            &item.reject,
                            "failed to marshal handler result",
                        );
                    }
                },
                Err(error) => {
                    let message = js_error_message(&error, &mut self.context);
                    record_callback_error(&mut self.context, "ipcMain.handle callback", &error);
                    reject_with_message(renderer, &item.reject, &message);
                }
            }
            renderer.run_jobs("ipcRenderer.invoke continuations");
        }

        for item in sends {
            pumped += 1;
            let listeners = shared
                .0
                .borrow()
                .ipc_listeners
                .get(&item.channel)
                .cloned()
                .unwrap_or_default();
            if listeners.is_empty() {
                continue;
            }
            let mut call_args = Vec::with_capacity(item.args.len() + 1);
            call_args.push(JsValue::from(
                ObjectInitializer::new(&mut self.context).build(),
            ));
            for arg in &item.args {
                match json_to_js(arg, &mut self.context) {
                    Ok(value) => call_args.push(value),
                    Err(error) => {
                        record_callback_error(
                            &mut self.context,
                            "ipcRenderer.send argument",
                            &error,
                        );
                        call_args.push(JsValue::null());
                    }
                }
            }
            for listener in listeners {
                if let Err(error) =
                    listener.call(&JsValue::undefined(), &call_args, &mut self.context)
                {
                    record_callback_error(&mut self.context, "ipcMain.on listener", &error);
                }
            }
        }
        if pumped > 0 {
            self.run_jobs("ipc microtasks");
        }
        pumped
    }

    /// Dispatch a notification click (issue #92): mark the delivery clicked
    /// and fire its renderer `onclick` with a `{ type: 'click' }` event.
    /// Returns `false` (no dispatch) for unknown delivery ids. Call on the
    /// renderer runtime whose context owns the handler.
    pub(crate) fn dispatch_notification_click(&mut self, id: u64) -> bool {
        let Some(shared) = self.context.get_data::<SharedElectronHost>().cloned() else {
            return false;
        };
        if !shared.0.borrow().notifications.click(id) {
            return false;
        }
        let handler = shared.0.borrow().notification_clicks.get(&id).cloned();
        let Some(handler) = handler else {
            return true;
        };
        let mut init = ObjectInitializer::new(&mut self.context);
        init.property(
            js_string!("type"),
            JsValue::from(js_string!("click")),
            Attribute::all(),
        );
        let event = JsValue::from(init.build());
        if let Err(error) = handler.call(&JsValue::undefined(), &[event], &mut self.context) {
            record_callback_error(&mut self.context, "Notification.onclick", &error);
        }
        self.run_jobs("notification click microtasks");
        true
    }

    /// Dispatch queued power events to main-scope `powerMonitor.on`
    /// listeners (issue #93 probe): each event invokes its name's listeners
    /// with a `{ type }` event object. Returns the number of dispatched
    /// events. Call on the main runtime whose context owns the listeners.
    pub(crate) fn dispatch_power_events(&mut self) -> usize {
        let Some(shared) = self.context.get_data::<SharedElectronHost>().cloned() else {
            return 0;
        };
        let pending = shared.0.borrow().power.take_pending();
        let count = pending.len();
        for event in pending {
            let name = power_event_name(event).to_string();
            let listeners = shared
                .0
                .borrow()
                .power_listeners
                .get(&name)
                .cloned()
                .unwrap_or_default();
            if listeners.is_empty() {
                continue;
            }
            let mut init = ObjectInitializer::new(&mut self.context);
            init.property(
                js_string!("type"),
                JsValue::from(js_string!(name.as_str())),
                Attribute::all(),
            );
            let call_args = [JsValue::from(init.build())];
            for listener in listeners {
                if let Err(error) =
                    listener.call(&JsValue::undefined(), &call_args, &mut self.context)
                {
                    record_callback_error(&mut self.context, "powerMonitor.on listener", &error);
                }
            }
        }
        if count > 0 {
            self.run_jobs("power event microtasks");
        }
        count
    }
}
