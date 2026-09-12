//! `require('electron')` for main-process scripts (issue #81, Slice 1).
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
//! * Closing the last window fires JS `window-all-closed` listeners and feeds
//!   the count into `App` (Electron's default quit).
//!
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
};
use strake_electron_compat::{App, BrowserWindowOptions, WindowManager};

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
