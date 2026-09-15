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
//!   `height`, `show`, `title`, `resizable`, `webPreferences.preload` are
//!   honoured — the preload path is recorded for the embedder, issue #109;
//!   the rest is accepted and ignored);
//!   `BrowserWindow.getAllWindows()` rehydrates one facade per live window
//!   (issue #107); `loadFile`/`loadURL`/`show`/`close`/`on('closed')`/
//!   `setResizable`/`isVisible`/`setBounds`/`getBounds` drive it, `webContents.send` queues
//!   main-to-renderer payloads per window (issue #91) and
//!   `webContents.getTitle` serves the synced page title (issue #90);
//!   `screen.*` serves the host display snapshot (issue #96);
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
use std::path::PathBuf;
use std::rc::Rc;

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::{JsArray, JsFunction, JsPromise, JsUint8Array};
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source,
    js_string, script::Script,
};
use strake_electron_compat::{
    App, AppPath, Bounds, BrowserWindowOptions, Clipboard, Enforcer, NativeTheme,
    NotificationCenter, NotificationRequest, PermissionManifest, PowerHub, PowerSaveBlockerKind,
    PrivilegedScheme, ProtocolRegistry, SafeStorage, Screen, SessionId, SessionRegistry,
    ThemeSource, WindowManager,
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
        setName(name) {
            globalThis.__strake_electron_app_set_name(name);
        },
        setAsDefaultProtocolClient(protocol) {
            return globalThis.__strake_electron_app_set_as_default_protocol_client(protocol);
        },
        setAppUserModelId(id) {
            globalThis.__strake_electron_app_set_app_user_model_id(id);
        },
        getAppPath() {
            return globalThis.__strake_electron_app_get_app_path();
        },
        getPath(name) {
            return globalThis.__strake_electron_app_get_path(name);
        },
        setPath(name, value) {
            globalThis.__strake_electron_app_set_path(name, value);
        },
    };
    electron.BrowserWindow = class BrowserWindow {
        constructor(options) {
            BrowserWindow.__strakeInitInstance(
                this,
                globalThis.__strake_electron_window_create(options || {}),
            );
        }
        static getAllWindows() {
            return globalThis.__strake_electron_windows_all().map((id) => {
                const win = Object.create(BrowserWindow.prototype);
                BrowserWindow.__strakeInitInstance(win, id);
                return win;
            });
        }
        static __strakeInitInstance(self, id) {
            self.__strakeWindowId = id;
            self.webContents = {
                send(channel, ...args) {
                    globalThis.__strake_electron_window_web_contents_send(id, channel, ...args);
                },
                getTitle() {
                    return globalThis.__strake_electron_window_get_title(id);
                },
                on(event, listener) {
                    globalThis.__strake_electron_window_web_contents_on(id, event, listener);
                    return this;
                },
                setWindowOpenHandler(handler) {
                    globalThis.__strake_electron_window_web_contents_set_window_open_handler(
                        id,
                        handler,
                    );
                },
                session: __strakeSessionForId(
                    globalThis.__strake_electron_window_get_session_id(id),
                ),
            };
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
        hide() {
            globalThis.__strake_electron_window_hide(this.__strakeWindowId);
        }
        on(event, listener) {
            globalThis.__strake_electron_window_on(this.__strakeWindowId, event, listener);
            return this;
        }
        close() {
            globalThis.__strake_electron_window_close(this.__strakeWindowId);
        }
        setResizable(flag) {
            globalThis.__strake_electron_window_set_resizable(this.__strakeWindowId, flag);
        }
        isVisible() {
            return globalThis.__strake_electron_window_is_visible(this.__strakeWindowId);
        }
        setBounds(rect) {
            globalThis.__strake_electron_window_set_bounds(this.__strakeWindowId, rect);
        }
        getBounds() {
            return globalThis.__strake_electron_window_get_bounds(this.__strakeWindowId);
        }
    };
    electron.nativeTheme = {
        get shouldUseDarkColors() {
            return globalThis.__strake_electron_native_theme_should_use_dark_colors();
        },
        get themeSource() {
            return globalThis.__strake_electron_native_theme_get_source();
        },
        set themeSource(value) {
            globalThis.__strake_electron_native_theme_set_source(value);
        },
    };
    electron.screen = {
        getPrimaryDisplay() {
            return globalThis.__strake_electron_screen_get_primary_display();
        },
        getAllDisplays() {
            return globalThis.__strake_electron_screen_get_all_displays();
        },
        getDisplayMatching(rect) {
            return globalThis.__strake_electron_screen_get_display_matching(rect || {});
        },
        getDisplayNearestPoint(point) {
            return globalThis.__strake_electron_screen_get_display_nearest_point(point || {});
        },
    };
    electron.ipcMain = {
        handle(channel, handler) {
            globalThis.__strake_electron_ipc_handle(channel, handler);
        },
        on(channel, listener) {
            globalThis.__strake_electron_ipc_on(channel, listener);
        },
    };
    electron.protocol = {
        registerSchemesAsPrivileged(customSchemes) {
            globalThis.__strake_electron_protocol_register_schemes_as_privileged(customSchemes);
        },
    };
    // Session wrappers are cached by id so the same underlying session is
    // always the same JS object (`session.fromPath(p) === webPreferences
    // session`, matching Electron).
    const __strakeSessions = new Map();
    function __strakeSessionForId(id) {
        let session = __strakeSessions.get(id);
        if (session === undefined) {
            session = {
                __strakeSessionId: id,
                protocol: {
                    handle(scheme, handler) {
                        globalThis.__strake_electron_session_protocol_handle(id, scheme, handler);
                    },
                },
                webRequest: {
                    onBeforeSendHeaders(filter, listener) {
                        globalThis.__strake_electron_session_web_request_on_before_send_headers(
                            id,
                            filter,
                            listener,
                        );
                    },
                    onHeadersReceived(filterOrListener, maybeListener) {
                        const hasFilter = typeof filterOrListener !== 'function';
                        globalThis.__strake_electron_session_web_request_on_headers_received(
                            id,
                            hasFilter ? filterOrListener : undefined,
                            hasFilter ? maybeListener : filterOrListener,
                        );
                    },
                },
            };
            __strakeSessions.set(id, session);
        }
        return session;
    }
    electron.session = {
        get defaultSession() {
            return __strakeSessionForId(globalThis.__strake_electron_session_default());
        },
        fromPath(path, options) {
            const cache = options && options.cache !== undefined ? !!options.cache : true;
            return __strakeSessionForId(
                globalThis.__strake_electron_session_from_path(path, cache),
            );
        },
        fromPartition(partition, options) {
            const cache = options && options.cache !== undefined ? !!options.cache : true;
            return __strakeSessionForId(
                globalThis.__strake_electron_session_from_partition(partition, cache),
            );
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

/// Node.js core stand-ins for main-process scripts (issue #108).
///
/// Real `main.js` files require `node:path` (`path.join(__dirname, ...)` is
/// line 3 of the minimal template), read `process.platform`/`process.versions`
/// guards, and call `url.format`. The pure modules below (`path`, `url`,
/// `process`, `events`, `constants`, `buffer`, `stream`, `util`) evaluate
/// here; the real `fs` shell evaluates on top in main-process installs only
/// (issue #154) — renderer contexts keep resolving `fs` to "Cannot find
/// module". Anything else (notably native addons, issue #144) still throws
/// Node's "Cannot find module" error.
///
/// Values are documented stand-ins, seeded from `__strake_node_info`
/// (`{ platform, versions, appRoot, env, processType }`, installed natively
/// per context):
/// `platform` follows Node's names (`darwin`/`win32`/`linux`), `versions`
/// carries Strake-marked strings until a real Node ABI exists, and
/// `__dirname` defaults to the app root (per-file module semantics need the
/// issue #16 loader; the `#110` runner sets the app root before eval).
const NODE_STANDIN_BOOTSTRAP_JS: &str = r#"
(function () {
    const info = globalThis.__strake_node_info || {};
    const versions = info.versions || {};
    // Issue #155: `path` was POSIX-only, so on Windows a backslash path
    // like `C:\app` misread as relative (`..` popped the drive) and
    // `path.join(__dirname, "..", ...)` silently stayed under the app
    // root. Win32 parsing (both separators, drive roots, root clamp)
    // applies when the host platform is win32; output stays `/`-joined,
    // matching the pinned `sep: "/"`. Drive-relative `C:foo` (a Windows
    // fossil) treats the drive as an un-poppable root.
    const WIN = info.platform === "win32";
    const split = (p) => String(p).split(WIN ? /[/\\]/ : "/").filter((seg) => seg.length > 0);
    const normalizeSegs = (segs, absolute) => {
        const out = [];
        for (const seg of segs) {
            if (seg === "." || seg === "") continue;
            if (seg === "..") {
                if (out.length > 0 && out[out.length - 1] !== "..") out.pop();
                else if (!absolute) out.push("..");
            } else {
                out.push(seg);
            }
        }
        return out;
    };
    const join = (...parts) => {
        const flat = parts.map((p) => String(p)).join("/");
        let drive = "";
        let rest = flat;
        let absolute = rest.startsWith("/");
        let unc = false;
        if (WIN) {
            const m = rest.match(/^([A-Za-z]:)([/\\]|$)/);
            if (m) {
                drive = m[1];
                rest = rest.slice(m[0].length);
                if (m[2] !== "") absolute = true;
            } else if (/^[/\\]/.test(rest)) {
                absolute = true;
                unc = /^[/\\][/\\]/.test(rest);
            }
        }
        const segs = normalizeSegs(split(rest), absolute || drive !== "");
        const body = segs.join("/");
        if (drive !== "") {
            if (absolute) return body === "" ? drive + "/" : drive + "/" + body;
            return body === "" ? drive : drive + body;
        }
        if (absolute) return "/" + (unc ? "/" + body : body);
        return body === "" ? "." : body;
    };
    const dirname = (p) => {
        const s = String(p);
        const stripped = WIN ? s.replace(/[/\\]+$/, "") : s.replace(/\/+$/, "");
        if (WIN && /^[A-Za-z]:$/.test(stripped)) return stripped + "/";
        const idx = WIN
            ? Math.max(stripped.lastIndexOf("/"), stripped.lastIndexOf("\\"))
            : stripped.lastIndexOf("/");
        if (idx < 0) return ".";
        if (idx === 0) return "/";
        let out = stripped.slice(0, idx);
        if (WIN) {
            out = out.replace(/\\/g, "/");
            if (/^[A-Za-z]:$/.test(out)) out += "/";
        }
        return out;
    };
    const basename = (p, ext) => {
        let s = String(p);
        s = WIN ? s.replace(/[/\\]+$/, "") : s.replace(/\/+$/, "");
        let base = (WIN ? s.split(/[/\\]/) : s.split("/")).pop() || "";
        if (WIN && /^[A-Za-z]:$/.test(base)) base = "";
        if (ext && base.endsWith(ext)) base = base.slice(0, base.length - ext.length);
        return base;
    };
    const pathModule = {
        join,
        normalize: (p) => join(String(p)),
        dirname,
        basename,
        isAbsolute: (p) => {
            const s = String(p);
            if (s.startsWith("/")) return true;
            if (!WIN) return false;
            return s.startsWith("\\") || /^[A-Za-z]:[/\\]/.test(s);
        },
        sep: "/",
        delimiter: ":",
    };
    const urlModule = {
        format(obj) {
            if (typeof obj === "string") return obj;
            const o = obj || {};
            if (o.href) return o.href;
            let out = o.protocol || "";
            if (!out.endsWith(":") && out !== "") out += ":";
            const host = o.host || o.hostname || "";
            if (o.slashes || (host !== "" && out.startsWith("file"))) out += "//";
            else if (host !== "") out += "//";
            out += host;
            out += o.pathname || o.path || "";
            return out;
        },
    };
    // Host environment snapshot (plain string map): `process.env.FOO`
    // reads and feature flags (`JOPLIN_SOURCE_MAP_DISABLED`) work as in
    // Node; writes stay on the snapshot and never touch the host. On
    // Windows the OS (and Node) resolve names case-insensitively (`Path`
    // answers `PATH`), so the snapshot gets a case-insensitive Proxy there
    // while enumeration keeps the exact-case keys.
    let processEnv = info.env || {};
    if ((info.platform || "linux") === "win32") {
        // Null-prototype map: a hostile `__proto__` lookup must miss
        // instead of hitting `Object.prototype`.
        const canonicalKey = Object.create(null);
        for (const key of Object.keys(processEnv)) canonicalKey[key.toLowerCase()] = key;
        processEnv = new Proxy(processEnv, {
            get(target, prop, receiver) {
                if (typeof prop === "string" && !(prop in target)) {
                    const hit = canonicalKey[prop.toLowerCase()];
                    return hit === undefined ? undefined : target[hit];
                }
                return target[prop];
            },
            set(target, prop, value) {
                if (typeof prop === "string" && !(prop in target)) {
                    const hit = canonicalKey[prop.toLowerCase()];
                    if (hit !== undefined) {
                        target[hit] = value;
                        return true;
                    }
                    canonicalKey[prop.toLowerCase()] = String(prop);
                }
                target[prop] = value;
                return true;
            },
            has(target, prop) {
                if (typeof prop === "string" && !(prop in target)) {
                    return canonicalKey[prop.toLowerCase()] !== undefined;
                }
                return prop in target;
            },
            deleteProperty(target, prop) {
                if (typeof prop === "string") {
                    const hit = canonicalKey[prop.toLowerCase()];
                    if (hit !== undefined) {
                        delete target[hit];
                        delete canonicalKey[prop.toLowerCase()];
                        return true;
                    }
                }
                delete target[prop];
                return true;
            },
        });
    }
    const processModule = {
        platform: info.platform || "linux",
        // Version-sniffing loaders (`graceful-fs` et al.) read
        // `process.version`, not `process.versions` (issue #154).
        version: "v" + (versions.node || "0.0.0-strake"),
        versions: {
            node: versions.node || "0.0.0-strake",
            chrome: versions.chrome || "0.0.0-strake",
            electron: versions.electron || "0.0.0-strake",
        },
        argv: [],
        env: processEnv,
        cwd() {
            return info.appRoot || "/";
        },
        // Warn without throwing (issue #155): real shims (`fs-extra`)
        // call this when `fs.realpath.native` is absent. Mirrors Node's
        // `Warning: message [code]` stderr line; the `warning` event and
        // inspection options are out of scope.
        emitWarning(warning, type, code) {
            const text = warning instanceof Error ? warning.stack || warning.message : String(warning);
            const label = type === undefined ? "Warning" : String(type);
            const suffix = code === undefined ? "" : " [" + String(code) + "]";
            console.error(label + ": " + text + suffix);
        },
        // FIFO microtask approximation (issue #155): callbacks run before
        // the next macrotask, in registration order. True Node priority
        // (nextTick ahead of already-queued promise jobs) is out of scope.
        nextTick(callback, ...args) {
            if (typeof callback !== "function") {
                throw new TypeError("callback must be a function");
            }
            queueMicrotask(() => callback(...args));
        },
    };
    // Electron process flavor (issue #155): main contexts report
    // `browser`, renderer contexts `renderer`, so entry-point guards
    // (`@sentry/electron`) take the right branch. Absent when the seeder
    // did not provide one, keeping the plain-Node shape.
    if (info.processType !== undefined) {
        processModule.type = info.processType;
    }
    // Wall-clock `performance` (issue #155): global in Node 16+ and
    // Electron main; Joplin's Sentry init calls `now()` during boot.
    // Durations are valid; monotonicity across NTP jumps is out of scope.
    const performanceTimeOrigin = Date.now();
    const performanceModule = {
        timeOrigin: performanceTimeOrigin,
        now() {
            return Date.now() - performanceTimeOrigin;
        },
    };
    // `node:timers` (issue #155): the runtime's timer globals, which also
    // back bare `setTimeout`/`clearTimeout`/`setInterval`/`clearInterval`.
    // `setImmediate`/`clearImmediate` and `timers/promises` are out of scope.
    const timersModule = {
        setTimeout: globalThis.setTimeout,
        clearTimeout: globalThis.clearTimeout,
        setInterval: globalThis.setInterval,
        clearInterval: globalThis.clearInterval,
    };
    const eventsModule = (() => {
        // Minimal `node:events` EventEmitter (issue #16 canary slice):
        // on/once/off/removeListener/removeAllListeners/emit/listenerCount/
        // listeners with Node's copy-on-emit and once-wrapper semantics.
        function EventEmitter() {
            this._events = Object.create(null);
        }
        EventEmitter.prototype._list = function (type) {
            const key = String(type);
            const list = this._events[key];
            return list === undefined ? null : list;
        };
        EventEmitter.prototype.on = function (type, listener) {
            if (typeof listener !== "function") throw new TypeError("listener must be a function");
            const key = String(type);
            if (this._events[key] === undefined) this._events[key] = [];
            this._events[key].push(listener);
            return this;
        };
        EventEmitter.prototype.addListener = EventEmitter.prototype.on;
        EventEmitter.prototype.once = function (type, listener) {
            if (typeof listener !== "function") throw new TypeError("listener must be a function");
            const self = this;
            const wrapper = function (...args) {
                self.removeListener(type, wrapper);
                return listener.apply(self, args);
            };
            wrapper.listener = listener;
            return this.on(type, wrapper);
        };
        EventEmitter.prototype.removeListener = function (type, listener) {
            const list = this._list(type);
            if (list !== null) {
                const key = String(type);
                this._events[key] = list.filter(
                    (fn) => fn !== listener && fn.listener !== listener
                );
            }
            return this;
        };
        EventEmitter.prototype.off = EventEmitter.prototype.removeListener;
        EventEmitter.prototype.removeAllListeners = function (type) {
            if (type === undefined) this._events = Object.create(null);
            else delete this._events[String(type)];
            return this;
        };
        EventEmitter.prototype.emit = function (type, ...args) {
            const list = this._list(type);
            if (list === null || list.length === 0) return false;
            for (const fn of list.slice()) fn.apply(this, args);
            return true;
        };
        EventEmitter.prototype.listeners = function (type) {
            const list = this._list(type);
            return list === null ? [] : list.slice();
        };
        EventEmitter.prototype.listenerCount = function (type) {
            const list = this._list(type);
            return list === null ? 0 : list.length;
        };
        return {
            EventEmitter,
            listenerCount: (emitter, type) => emitter.listenerCount(type),
        };
    })();
    // `process` is an EventEmitter in Node (issue #155): real apps install
    // `unhandledRejection`/`uncaughtException` handlers at load via
    // `process.on(...)`. Same construction idiom as `Domain` above; the
    // module table keeps aliasing this same object.
    eventsModule.EventEmitter.call(processModule);
    Object.setPrototypeOf(processModule, eventsModule.EventEmitter.prototype);
    // Load-time `constants` subset (issue #154): the POSIX-stable values
    // plus the platform-varying `O_*` flags `graceful-fs` reads at load.
    // Exotic flags (`O_DIRECTORY`, `O_SYNC`, …) land on demand.
    const constantsModule = (() => {
        const platform = info.platform || "linux";
        const isWindows = platform === "win32";
        const isMac = platform === "darwin";
        return {
            O_RDONLY: 0,
            O_WRONLY: 1,
            O_RDWR: 2,
            O_CREAT: isWindows ? 256 : isMac ? 512 : 64,
            O_EXCL: isWindows ? 1024 : isMac ? 2048 : 128,
            O_TRUNC: isWindows ? 512 : isMac ? 1024 : 512,
            O_APPEND: isWindows ? 8 : isMac ? 8 : 1024,
            F_OK: 0,
            R_OK: 4,
            W_OK: 2,
            X_OK: 1,
            COPYFILE_EXCL: 1,
            COPYFILE_FICLONE: 2,
            COPYFILE_FICLONE_FORCE: 4,
            S_IFMT: 61440,
            S_IFREG: 32768,
            S_IFDIR: 16384,
            S_IFLNK: 40960,
            S_IFCHR: 8192,
            S_IFBLK: 24576,
            S_IFIFO: 4096,
            S_IFSOCK: 49152,
        };
    })();
    // Minimal `Buffer` over `Uint8Array` (issue #154): alloc, from
    // string/bytes, toString, length, slice. Encoding work rides the
    // runtime's `TextEncoder`/`TextDecoder`/`atob`/`btoa`; hex and base64
    // stay manual so exotic labels never matter.
    const bufferModule = (() => {
        const textEncoder = new TextEncoder();
        const textDecoder = new TextDecoder();
        let latin1Decoder = null;
        try {
            latin1Decoder = new TextDecoder("latin1");
        } catch (e) {
            latin1Decoder = null;
        }
        const HEX = "0123456789abcdef";
        const hexEncode = (view) => {
            let out = "";
            for (let i = 0; i < view.length; i++) {
                out += HEX[(view[i] >> 4) & 15] + HEX[view[i] & 15];
            }
            return out;
        };
        const hexDecode = (text) => {
            const clean = String(text).replace(/\s+/g, "");
            const bytes = [];
            for (let i = 0; i + 1 < clean.length; i += 2) {
                const byte = parseInt(clean.slice(i, i + 2), 16);
                if (Number.isNaN(byte)) break;
                bytes.push(byte);
            }
            return bytes;
        };
        const base64Encode = (view) => {
            let binary = "";
            for (let i = 0; i < view.length; i += 8192) {
                binary += String.fromCharCode.apply(
                    null,
                    Array.prototype.slice.call(view.subarray(i, i + 8192))
                );
            }
            return btoa(binary);
        };
        const base64Decode = (text) => {
            const binary = atob(String(text));
            const bytes = new Array(binary.length);
            for (let i = 0; i < binary.length; i++) {
                bytes[i] = binary.charCodeAt(i) & 255;
            }
            return bytes;
        };
        const normalizeEncoding = (encoding) => {
            const name = String(encoding || "utf8").toLowerCase().replace(/[-_]/g, "");
            if (name === "utf8") return "utf8";
            return name;
        };
        const encodeString = (text, encoding) => {
            const name = normalizeEncoding(encoding);
            if (name === "utf8") return Array.from(textEncoder.encode(String(text)));
            if (name === "hex") return hexDecode(text);
            if (name === "base64") return base64Decode(text);
            if (name === "base64url") {
                return base64Decode(
                    String(text).replace(/-/g, "+").replace(/_/g, "/")
                );
            }
            if (name === "latin1" || name === "binary" || name === "ascii") {
                const s = String(text);
                const bytes = new Array(s.length);
                for (let i = 0; i < s.length; i++) bytes[i] = s.charCodeAt(i) & 255;
                return bytes;
            }
            throw new TypeError("Unknown encoding: " + encoding);
        };
        class Buffer extends Uint8Array {
            // `allocUnsafe` (issue #155): Joplin's `uuid` parses namespace
            // UUIDs into one. Headless memory is always fresh, so this is
            // zero-filled — strictly safer than Node's pooled garbage, and
            // identical for callers that fill before reading.
            static allocUnsafe(size) {
                const count = Number(size);
                if (!Number.isInteger(count) || count < 0) {
                    throw new RangeError("Invalid buffer size: " + size);
                }
                return new Buffer(count);
            }
            static alloc(size, fill, encoding) {
                const count = Number(size);
                if (!Number.isInteger(count) || count < 0) {
                    throw new RangeError("Invalid buffer size: " + size);
                }
                const out = new Buffer(count);
                if (fill === undefined || fill === 0) return out;
                if (typeof fill === "number") {
                    out.fill(fill & 255);
                    return out;
                }
                const pattern = encodeString(String(fill), encoding || "utf8");
                if (pattern.length === 0) return out;
                for (let i = 0; i < out.length; i++) out[i] = pattern[i % pattern.length];
                return out;
            }
            static from(value, encoding) {
                if (typeof value === "string") return new Buffer(encodeString(value, encoding || "utf8"));
                if (typeof value === "number") {
                    throw new TypeError("The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array.");
                }
                if (Array.isArray(value)) return new Buffer(value);
                if (value instanceof ArrayBuffer) return new Buffer(value);
                if (value && ArrayBuffer.isView(value)) {
                    return new Buffer(value.buffer, value.byteOffset, value.byteLength);
                }
                throw new TypeError("The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array.");
            }
            static isBuffer(value) {
                return value instanceof Buffer;
            }
            toString(encoding, start, end) {
                const name = normalizeEncoding(encoding || "utf8");
                const view = this.subarray(start === undefined ? 0 : start, end === undefined ? this.length : end);
                if (name === "utf8") return textDecoder.decode(view);
                if (name === "hex") return hexEncode(view);
                if (name === "base64") return base64Encode(view);
                if (name === "base64url") {
                    return base64Encode(view).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
                }
                if (name === "latin1" || name === "binary" || name === "ascii") {
                    if (latin1Decoder !== null) return latin1Decoder.decode(view);
                    let out = "";
                    for (let i = 0; i < view.length; i++) out += String.fromCharCode(view[i]);
                    return out;
                }
                throw new TypeError("Unknown encoding: " + encoding);
            }
        }
        return { Buffer };
    })();
    // `string_decoder` (issue #155): Joplin's streaming parsers hold one
    // across `write()` calls, so split sequences buffer. `utf8` rides the
    // runtime `TextDecoder` in streaming mode (incomplete tails flush as
    // U+FFFD, matching Node); the other text shapes buffer manually since
    // their quanta are trivial (`utf16le` pairs, single bytes) or are not
    // text labels at all (`base64` quartets, `hex` pairs).
    const stringDecoderModule = (() => {
        const unknownEncoding = (label) => {
            const error = new TypeError(`Unknown encoding: ${label}`);
            error.code = "ERR_UNKNOWN_ENCODING";
            return error;
        };
        const toBytes = (data) => {
            if (typeof data === "string") return Buffer.from(data, "utf8");
            if (data instanceof ArrayBuffer) return new Buffer(data);
            if (data && ArrayBuffer.isView(data)) {
                return new Buffer(data.buffer, data.byteOffset, data.byteLength);
            }
            throw new TypeError("data must be a string, Buffer, TypedArray, DataView, or ArrayBuffer");
        };
        const binaryString = (bytes) => {
            let out = "";
            for (let i = 0; i < bytes.length; i += 8192) {
                out += String.fromCharCode.apply(null, Array.prototype.slice.call(bytes.subarray(i, i + 8192)));
            }
            return out;
        };
        function StringDecoder(encoding) {
            if (!(this instanceof StringDecoder)) return new StringDecoder(encoding);
            const label = encoding === undefined ? "utf8" : String(encoding).toLowerCase().replace(/[-_]/g, "");
            if (label === "utf8" || label === "utf") {
                this.encoding = "utf8";
                // The runtime `TextDecoder` finalizes every call (no
                // streaming option), so the incomplete tail is held here
                // and only complete prefixes decode — flushing as U+FFFD.
                this._text = new TextDecoder("utf-8");
                this._held = [];
            } else if (label === "utf16le" || label === "ucs2") {
                this.encoding = "utf16le";
                this._held = [];
            } else if (label === "ascii" || label === "latin1" || label === "binary") {
                this.encoding = label === "ascii" ? "ascii" : "latin1";
                this._single = label === "ascii" ? 127 : 255;
            } else if (label === "base64" || label === "hex") {
                this.encoding = label;
                this._tail = "";
            } else {
                throw unknownEncoding(encoding === undefined ? "undefined" : String(encoding));
            }
        }
        StringDecoder.prototype.write = function (buffer) {
            const view = toBytes(buffer);
            if (this._text !== undefined) {
                const bytes = new Buffer(this._held.length + view.length);
                bytes.set(this._held, 0);
                bytes.set(view, this._held.length);
                let i = bytes.length - 1;
                while (i >= 0 && bytes[i] >= 0x80 && bytes[i] < 0xc0) i--;
                let cut = bytes.length;
                if (i >= 0) {
                    const lead = bytes[i];
                    let need = 0;
                    if (lead >= 0xc2 && lead < 0xe0) need = 2;
                    else if (lead >= 0xe0 && lead < 0xf0) need = 3;
                    else if (lead >= 0xf0 && lead < 0xf5) need = 4;
                    if (need > 0 && bytes.length - i < need) cut = i;
                }
                this._held = Array.prototype.slice.call(bytes.subarray(cut));
                return this._text.decode(bytes.subarray(0, cut));
            }
            if (this._single !== undefined) {
                const mask = this._single;
                let out = "";
                for (let i = 0; i < view.length; i++) out += String.fromCharCode(view[i] & mask);
                return out;
            }
            if (this.encoding === "utf16le") {
                const units = this._held;
                for (let i = 0; i < view.length; i++) units.push(view[i]);
                let out = "";
                const even = units.length - (units.length % 2);
                for (let i = 0; i < even; i += 2) out += String.fromCharCode(units[i] | (units[i + 1] << 8));
                this._held = units.slice(even);
                return out;
            }
            const text = this._tail + binaryString(view);
            if (this.encoding === "base64") {
                const even = text.length - (text.length % 4);
                this._tail = text.slice(even);
                return even === 0 ? "" : binaryString(Buffer.from(text.slice(0, even), "base64"));
            }
            const even = text.length - (text.length % 2);
            this._tail = text.slice(even);
            let out = "";
            for (let i = 0; i < even; i += 2) out += String.fromCharCode(parseInt(text.slice(i, i + 2), 16));
            return out;
        };
        StringDecoder.prototype.end = function (buffer) {
            let out = buffer === undefined ? "" : this.write(buffer);
            if (this._text !== undefined) {
                const tail = this._held;
                this._held = [];
                return out + this._text.decode(new Buffer(tail));
            }
            if (this.encoding === "utf16le") {
                const leftover = this._held.length > 0;
                this._held = [];
                return out + (leftover ? "�" : "");
            }
            if (this.encoding === "base64" || this.encoding === "hex") {
                const tail = this._tail;
                this._tail = "";
                if (tail === "") return out;
                try {
                    if (this.encoding === "base64") {
                        const padded = tail + "=".repeat((4 - (tail.length % 4)) % 4);
                        return out + binaryString(Buffer.from(padded, "base64"));
                    }
                    return out;
                } catch (e) {
                    return out;
                }
            }
            return out;
        };
        return { StringDecoder };
    })();
    // `stream` classes (issues #154/#155): `Stream` base with `pipe`, plus
    // the `Readable`/`Writable`/`Duplex`/`Transform`/`PassThrough` family
    // real updaters subclass (`class extends Transform`). Plain-function
    // constructors (never classes): third-party shims wrap constructors via
    // `.apply`, which throws on classes. Callbacks complete on the microtask
    // queue, never synchronously. File-backed `fs` streams live on the `fs`
    // shell where the native primitives are.
    const streamModule = (() => {
        class Stream extends eventsModule.EventEmitter {
            pipe(dest) {
                this.on("data", (chunk) => dest.write(chunk));
                this.on("end", () => {
                    if (typeof dest.end === "function") dest.end();
                });
                return dest;
            }
        }
        const notImplemented = (method) => {
            const error = new Error(`The _${method}() method is not implemented`);
            error.code = "ERR_METHOD_NOT_IMPLEMENTED";
            return error;
        };
        function Readable(options) {
            if (!(this instanceof Readable)) return new Readable(options);
            eventsModule.EventEmitter.call(this);
            if (options && typeof options.read === "function") this._read = options.read;
            this._readableBuffer = [];
            this._readableEnded = false;
            this._readableFlowing = false;
            this._readableEndEmitted = false;
        }
        Readable.prototype = Object.create(Stream.prototype);
        Readable.prototype.constructor = Readable;
        Readable.prototype._read = function () {};
        Readable.prototype._flowBuffer = function () {
            while (this._readableFlowing && this._readableBuffer.length > 0) {
                this.emit("data", this._readableBuffer.shift());
            }
            this._emitReadableEnd();
        };
        Readable.prototype._emitReadableEnd = function () {
            if (this._readableEndEmitted || !this._readableEnded || this._readableBuffer.length > 0) return;
            this._readableEndEmitted = true;
            this.emit("end");
            this.emit("close");
        };
        Readable.prototype.push = function (chunk) {
            if (this._readableEnded) {
                const error = new Error("push after EOF");
                error.code = "ERR_STREAM_PUSH_AFTER_EOF";
                this.emit("error", error);
                return false;
            }
            if (chunk === null) {
                this._readableEnded = true;
                if (this._readableFlowing) this._emitReadableEnd();
                return false;
            }
            if (this._readableFlowing) this.emit("data", chunk);
            else this._readableBuffer.push(chunk);
            return true;
        };
        Readable.prototype.unshift = function (chunk) {
            this._readableBuffer.unshift(chunk);
            if (this._readableFlowing) this._flowBuffer();
        };
        Readable.prototype.read = function () {
            if (this._readableBuffer.length > 0) {
                const chunk = this._readableBuffer.shift();
                this._emitReadableEnd();
                return chunk;
            }
            return null;
        };
        Readable.prototype.pause = function () {
            this._readableFlowing = false;
            return this;
        };
        Readable.prototype.resume = function () {
            if (!this._readableFlowing) {
                this._readableFlowing = true;
                this._flowBuffer();
            }
            return this;
        };
        const flowOnData = (onOrOnce) => function (type, listener) {
            Stream.prototype[onOrOnce].call(this, type, listener);
            if (type === "data" && !this._readableFlowing) {
                this._readableFlowing = true;
                this._flowBuffer();
            }
            return this;
        };
        Readable.prototype.on = flowOnData("on");
        Readable.prototype.once = flowOnData("once");
        Readable.from = function (iterable) {
            const out = new Readable();
            queueMicrotask(async () => {
                try {
                    for await (const chunk of iterable) out.push(chunk);
                    out.push(null);
                } catch (error) {
                    out.emit("error", error);
                }
            });
            return out;
        };
        function Writable(options) {
            if (!(this instanceof Writable)) return new Writable(options);
            eventsModule.EventEmitter.call(this);
            if (options && typeof options.write === "function") this._write = options.write;
            if (options && typeof options.final === "function") this._final = options.final;
            this._pendingWrites = 0;
            this._writableEnded = false;
            this._writableFinished = false;
        }
        Writable.prototype = Object.create(Stream.prototype);
        Writable.prototype.constructor = Writable;
        Writable.prototype._write = function (chunk, encoding, callback) {
            callback(notImplemented("write"));
        };
        Writable.prototype._final = function (callback) {
            callback();
        };
        Writable.prototype.write = function (chunk, encoding, callback) {
            if (typeof encoding === "function") {
                callback = encoding;
                encoding = undefined;
            }
            const done = typeof callback === "function" ? callback : () => {};
            if (this._writableEnded) {
                const error = new Error("write after end");
                error.code = "ERR_STREAM_WRITE_AFTER_END";
                this.emit("error", error);
                queueMicrotask(() => done(error));
                return false;
            }
            this._pendingWrites++;
            queueMicrotask(() => {
                const finishWrite = (error) => {
                    this._pendingWrites--;
                    if (error) {
                        this.emit("error", error);
                        done(error);
                        return;
                    }
                    done();
                };
                try {
                    this._write(chunk, encoding === undefined ? "utf8" : encoding, finishWrite);
                } catch (error) {
                    finishWrite(error);
                }
            });
            return true;
        };
        Writable.prototype.end = function (chunk, encoding, callback) {
            if (typeof chunk === "function") {
                callback = chunk;
                chunk = undefined;
                encoding = undefined;
            } else if (typeof encoding === "function") {
                callback = encoding;
                encoding = undefined;
            }
            if (chunk !== undefined && chunk !== null) this.write(chunk, encoding);
            if (this._writableEnded) {
                if (typeof callback === "function") {
                    const error = new Error("end already called");
                    error.code = "ERR_STREAM_ALREADY_FINISHED";
                    queueMicrotask(() => callback(error));
                }
                return this;
            }
            this._writableEnded = true;
            const self = this;
            const finish = () => {
                if (self._pendingWrites > 0) {
                    queueMicrotask(finish);
                    return;
                }
                const done = (error) => {
                    if (error) {
                        self.emit("error", error);
                        if (typeof callback === "function") callback(error);
                        return;
                    }
                    if (!self._writableFinished) {
                        self._writableFinished = true;
                        self.emit("finish");
                    }
                    if (typeof callback === "function") callback();
                };
                try {
                    self._final(done);
                } catch (error) {
                    done(error);
                }
            };
            queueMicrotask(finish);
            return this;
        };
        function Duplex(options) {
            if (!(this instanceof Duplex)) return new Duplex(options);
            Readable.call(this, options);
            Writable.call(this, options);
        }
        Duplex.prototype = Object.create(Readable.prototype);
        Duplex.prototype.constructor = Duplex;
        Duplex.prototype.write = Writable.prototype.write;
        Duplex.prototype.end = Writable.prototype.end;
        function Transform(options) {
            if (!(this instanceof Transform)) return new Transform(options);
            Duplex.call(this, options);
            if (options && typeof options.transform === "function") this._transform = options.transform;
            if (options && typeof options.flush === "function") this._flush = options.flush;
        }
        Transform.prototype = Object.create(Duplex.prototype);
        Transform.prototype.constructor = Transform;
        Transform.prototype._transform = function (chunk, encoding, callback) {
            callback(notImplemented("transform"));
        };
        Transform.prototype._flush = function (callback) {
            callback();
        };
        // Node forwards the `_transform` callback's data argument
        // downstream — real updaters (`o(null, chunk)`) rely on it and never
        // push manually.
        Transform.prototype._write = function (chunk, encoding, callback) {
            const self = this;
            const transformDone = (error, data) => {
                if (error) {
                    callback(error);
                    return;
                }
                if (data !== undefined && data !== null) self.push(data);
                callback();
            };
            try {
                this._transform(chunk, encoding, transformDone);
            } catch (error) {
                transformDone(error);
            }
        };
        Transform.prototype._final = function (callback) {
            const self = this;
            const flushDone = (error) => {
                if (error) {
                    callback(error);
                    return;
                }
                self.push(null);
                callback();
            };
            try {
                this._flush(flushDone);
            } catch (error) {
                flushDone(error);
            }
        };
        function PassThrough(options) {
            if (!(this instanceof PassThrough)) return new PassThrough(options);
            Transform.call(this, options);
        }
        PassThrough.prototype = Object.create(Transform.prototype);
        PassThrough.prototype.constructor = PassThrough;
        PassThrough.prototype._transform = function (chunk, encoding, callback) {
            callback(null, chunk);
        };
        const pipeline = (...args) => {
            let callback = () => {};
            if (args.length > 0 && typeof args[args.length - 1] === "function") callback = args.pop();
            if (args.length < 2) throw new Error("pipeline requires at least two streams");
            let called = false;
            const done = (error) => {
                if (called) return;
                called = true;
                callback(error === undefined ? null : error);
            };
            for (let i = 0; i < args.length - 1; i++) {
                args[i].on("error", done);
                args[i].pipe(args[i + 1]);
            }
            const last = args[args.length - 1];
            last.on("error", done);
            last.on("finish", () => done(null));
            last.on("end", () => done(null));
            return last;
        };
        return { Stream, Readable, Writable, Duplex, Transform, PassThrough, pipeline };
    })();
    // `util` subset (issue #154): `format` (`%s %d %i %f %j %%`, leftovers
    // appended), `debuglog` (a `NODE_DEBUG`-gated logger), `inherits`.
    const utilModule = (() => {
        const inspectValue = (value) => {
            if (typeof value === "string") return value;
            try {
                const json = JSON.stringify(value);
                return json === undefined ? "undefined" : json;
            } catch (e) {
                return "[Circular]";
            }
        };
        const format = (fmt, ...args) => {
            if (typeof fmt !== "string") {
                return [fmt, ...args].map(inspectValue).join(" ");
            }
            let i = 0;
            const head = String(fmt).replace(/%[sdifjoO%]/g, (match) => {
                if (match === "%%") return "%";
                if (i >= args.length) return match;
                const arg = args[i++];
                if (match === "%s") return String(arg);
                if (match === "%d") return Number(arg).toString();
                if (match === "%i") return parseInt(arg, 10).toString();
                if (match === "%f") return parseFloat(arg).toString();
                return inspectValue(arg);
            });
            const tail = args.slice(i).map(inspectValue);
            return tail.length > 0 ? head + " " + tail.join(" ") : head;
        };
        const debuglog = (set) => {
            const name = String(set).toUpperCase();
            return (...args) => {
                const env = (globalThis.process && globalThis.process.env && globalThis.process.env.NODE_DEBUG) || "";
                const enabled = env === "*" || String(env).toUpperCase().split(/[,\s]+/).indexOf(name) !== -1;
                if (enabled) console.error(name + ": " + format(...args));
            };
        };
        const inherits = (ctor, superCtor) => {
            if (typeof ctor !== "function" || typeof superCtor !== "function") {
                throw new TypeError("inherits requires constructor functions");
            }
            Object.setPrototypeOf(ctor.prototype, superCtor.prototype);
            Object.setPrototypeOf(ctor, superCtor);
            ctor.super_ = superCtor;
        };
        // Warn-once wrapper (issue #155): the `debug` package deprecates
        // at import time, so this must exist before any call runs.
        const deprecate = (fn, message, code) => {
            if (typeof fn !== "function") {
                throw new TypeError("fn must be a function");
            }
            let warned = false;
            const deprecated = function (...args) {
                if (!warned) {
                    warned = true;
                    const text = "DeprecationWarning: " + String(message) + (code === undefined ? "" : " [" + String(code) + "]");
                    if (globalThis.process && typeof globalThis.process.emitWarning === "function") {
                        globalThis.process.emitWarning(message, "DeprecationWarning", code);
                    } else {
                        console.error(text);
                    }
                }
                return fn.apply(this, args);
            };
            return deprecated;
        };
        // `promisify` (issue #155): Joplin promisifies `fs.readFile` /
        // `fs.readdir` at import time. Honours a custom implementation via
        // `promisify.custom`; otherwise appends an errback that settles a
        // real promise (multi-value callbacks resolve an array, like Node).
        const promisify = (fn) => {
            if (typeof fn !== "function") {
                throw new TypeError("The \"original\" argument must be of type function");
            }
            if (typeof fn[promisify.custom] === "function") return fn[promisify.custom];
            function promisified(...args) {
                const self = this;
                return new Promise((resolve, reject) => {
                    fn.call(self, ...args, (error, ...values) => {
                        if (error) reject(error);
                        else resolve(values.length > 1 ? values : values[0]);
                    });
                });
            }
            Object.setPrototypeOf(promisified, Object.getPrototypeOf(fn));
            return promisified;
        };
        promisify.custom = Symbol("util.promisify.custom");
        // `TextEncoder`/`TextDecoder` re-exports (issue #155): Node exposes
        // the globals from `util`; real bundles `new` them at runtime.
        return {
            format,
            debuglog,
            inherits,
            deprecate,
            promisify,
            TextEncoder: globalThis.TextEncoder,
            TextDecoder: globalThis.TextDecoder,
        };
    })();
    // `assert` subset (issue #155): callable assertion plus the comparison
    // helpers real bundles use at import time, with Node's `ERR_ASSERTION`
    // shape (`code`, `actual`, `expected`, `operator`) on failure.
    const assertModule = (() => {
        class AssertionError extends Error {
            constructor(message, actual, expected, operator) {
                super(message === undefined ? "Assertion failed" : String(message));
                this.name = "AssertionError";
                this.code = "ERR_ASSERTION";
                this.actual = actual;
                this.expected = expected;
                this.operator = operator;
            }
        }
        const fail = (message, actual, expected, operator) => {
            throw new AssertionError(message, actual, expected, operator || "fail");
        };
        const isObject = (value) => (typeof value === "object" && value !== null) || typeof value === "function";
        const deepStrictEqualValues = (actual, expected, seen) => {
            if (Object.is(actual, expected)) return true;
            if (typeof actual !== "object" || typeof expected !== "object" || actual === null || expected === null) {
                return false;
            }
            for (const pair of seen) {
                if (pair[0] === actual && pair[1] === expected) return true;
            }
            seen.push([actual, expected]);
            if (Object.getPrototypeOf(actual) !== Object.getPrototypeOf(expected)) return false;
            if (actual instanceof Date || expected instanceof Date) {
                return actual instanceof Date && expected instanceof Date && actual.getTime() === expected.getTime();
            }
            if (actual instanceof RegExp || expected instanceof RegExp) {
                return actual instanceof RegExp && expected instanceof RegExp && actual.source === expected.source && actual.flags === expected.flags;
            }
            if (ArrayBuffer.isView(actual) || ArrayBuffer.isView(expected)) {
                if (!ArrayBuffer.isView(actual) || !ArrayBuffer.isView(expected)) return false;
                if (actual.byteLength !== expected.byteLength) return false;
                for (let i = 0; i < actual.byteLength; i++) {
                    if (actual[i] !== expected[i]) return false;
                }
                return true;
            }
            if (actual instanceof Map || expected instanceof Map || actual instanceof Set || expected instanceof Set) {
                if (!(actual instanceof Map) || !(expected instanceof Map)) {
                    if (!(actual instanceof Set) || !(expected instanceof Set)) return false;
                    if (actual.size !== expected.size) return false;
                    for (const item of actual) {
                        if (!expected.has(item)) return false;
                    }
                    return true;
                }
                if (actual.size !== expected.size) return false;
                for (const [key, value] of actual) {
                    if (!expected.has(key) || !deepStrictEqualValues(value, expected.get(key), seen)) return false;
                }
                return true;
            }
            const actualKeys = [...Object.keys(actual), ...Object.getOwnPropertySymbols(actual)];
            const expectedKeys = [...Object.keys(expected), ...Object.getOwnPropertySymbols(expected)];
            if (actualKeys.length !== expectedKeys.length) return false;
            for (const key of actualKeys) {
                if (!Object.prototype.propertyIsEnumerable.call(expected, key)) return false;
                if (!deepStrictEqualValues(actual[key], expected[key], seen)) return false;
            }
            return true;
        };
        function assert(value, message) {
            if (!value) fail(message, value, true, "==");
        }
        assert.ok = (value, message) => assert(value, message);
        assert.equal = (actual, expected, message) => {
            if (actual != expected) fail(message, actual, expected, "==");
        };
        assert.notEqual = (actual, expected, message) => {
            if (actual == expected) fail(message, actual, expected, "!=");
        };
        assert.strictEqual = (actual, expected, message) => {
            if (!Object.is(actual, expected)) fail(message, actual, expected, "strictEqual");
        };
        assert.notStrictEqual = (actual, expected, message) => {
            if (Object.is(actual, expected)) fail(message, actual, expected, "notStrictEqual");
        };
        assert.deepEqual = (actual, expected, message) => {
            if (!deepStrictEqualValues(actual, expected, [])) fail(message, actual, expected, "deepEqual");
        };
        assert.notDeepEqual = (actual, expected, message) => {
            if (deepStrictEqualValues(actual, expected, [])) fail(message, actual, expected, "notDeepEqual");
        };
        assert.deepStrictEqual = (actual, expected, message) => {
            if (!deepStrictEqualValues(actual, expected, [])) fail(message, actual, expected, "deepStrictEqual");
        };
        assert.notDeepStrictEqual = (actual, expected, message) => {
            if (deepStrictEqualValues(actual, expected, [])) fail(message, actual, expected, "notDeepStrictEqual");
        };
        assert.match = (value, regexp, message) => {
            if (!regexp.test(String(value))) fail(message, value, regexp, "match");
        };
        assert.doesNotMatch = (value, regexp, message) => {
            if (regexp.test(String(value))) fail(message, value, regexp, "doesNotMatch");
        };
        assert.throws = (fn, expected, message) => {
            if (typeof expected === "string") {
                message = expected;
                expected = undefined;
            }
            let thrown;
            try {
                fn();
            } catch (error) {
                thrown = error;
            }
            if (thrown === undefined) fail(message || "Missing expected exception", undefined, expected, "throws");
            if (expected !== undefined) {
                let valid = false;
                if (typeof expected === "function") {
                    // Constructors check `instanceof`; plain validator
                    // functions (no prototype) run against the error.
                    valid = expected.prototype !== undefined
                        ? thrown instanceof expected
                        : !!expected(thrown);
                } else if (expected instanceof RegExp) {
                    valid = expected.test(String((thrown && thrown.message) || thrown));
                } else if (isObject(expected)) {
                    valid = Object.keys(expected).every((key) => deepStrictEqualValues(thrown[key], expected[key], []));
                }
                if (!valid) fail(message, thrown, expected, "throws");
            }
        };
        assert.doesNotThrow = (fn, message) => {
            try {
                fn();
            } catch (error) {
                fail(message || "Got unwanted exception", error, undefined, "doesNotThrow");
            }
        };
        assert.fail = fail;
        assert.AssertionError = AssertionError;
        return assert;
    })();
    // `child_process` import-time surface (issue #155): real updater and
    // crash-reporter chunks bind `exec`/`spawn` at import time, but process
    // spawning is capability-gated follow-up (issue #16 model) — every op
    // fails loudly with Node's own unavailable-on-this-platform code
    // instead of faking success a caller might trust.
    const childProcessModule = (() => {
        const unavailable = (name) => {
            const error = new Error(`${name} is not available in this host (process spawning is capability-gated; see issue #16)`);
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = name;
            throw error;
        };
        return {
            exec: (...args) => unavailable("exec"),
            execFile: (...args) => unavailable("execFile"),
            spawn: (...args) => unavailable("spawn"),
            fork: (...args) => unavailable("fork"),
            execSync: (...args) => unavailable("execSync"),
            execFileSync: (...args) => unavailable("execFileSync"),
            spawnSync: (...args) => unavailable("spawnSync"),
        };
    })();
    // `crypto` hashing subset (issue #155): real digests, implemented in
    // pure JS with hardcoded constant tables (no float math, no new
    // dependencies — `Math.sin`-derived tables would depend on libm
    // rounding). md5/sha1/sha256, HMAC, and PBKDF2 are byte-exact per
    // their RFCs (committed vectors are the oracle). Entropy
    // (`randomBytes`, `randomUUID`) is real OS randomness via the native
    // `__strake_random_bytes` (`getrandom`, user-approved — Joplin seeds
    // `uuid` at import time). Ciphers stay coded-unavailable: fake crypto
    // would corrupt data and keys.
    const cryptoModule = (() => {
        const unavailableCrypto = (name) => {
            const error = new Error(`${name} is not available in this host (see issue #155)`);
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = name;
            throw error;
        };
        const cryptoBytes = (data, encoding) => {
            if (typeof data === "string") return [...Buffer.from(data, encoding === undefined ? "utf8" : encoding)];
            if (typeof data === "number") throw new TypeError("data must be a string or Uint8Array");
            if (Array.isArray(data)) return data.slice();
            if (data instanceof ArrayBuffer) return [...new Uint8Array(data)];
            if (data && ArrayBuffer.isView(data)) return [...new Uint8Array(data.buffer, data.byteOffset, data.byteLength)];
            throw new TypeError("data must be a string, Buffer, TypedArray, DataView, or Array");
        };
        const hexOfBytes = (bytes) => bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
        const rotl32 = (x, n) => ((x << n) | (x >>> (32 - n))) >>> 0;
        const MD5_S = [
            7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
            5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
            4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
            6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
        ];
        const MD5_K = [
            0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
            0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
            0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
            0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
            0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
            0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
            0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
            0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
        ];
        // MD5 (RFC 1321): little-endian padding and digest.
        const md5Digest = (bytes) => {
            const bitLen = bytes.length * 8;
            const padded = bytes.slice();
            padded.push(128);
            while (padded.length % 64 !== 56) padded.push(0);
            const lo = bitLen >>> 0;
            const hi = Math.floor(bitLen / 4294967296);
            padded.push(lo & 255, (lo >>> 8) & 255, (lo >>> 16) & 255, (lo >>> 24) & 255,
                hi & 255, (hi >>> 8) & 255, (hi >>> 16) & 255, (hi >>> 24) & 255);
            let a0 = 0x67452301, b0 = 0xefcdab89, c0 = 0x98badcfe, d0 = 0x10325476;
            const M = new Array(16);
            for (let off = 0; off < padded.length; off += 64) {
                for (let i = 0; i < 16; i++) {
                    M[i] = (padded[off + i * 4] | (padded[off + i * 4 + 1] << 8) | (padded[off + i * 4 + 2] << 16) | (padded[off + i * 4 + 3] << 24)) >>> 0;
                }
                let A = a0, B = b0, C = c0, D = d0;
                for (let i = 0; i < 64; i++) {
                    let F, g;
                    if (i < 16) { F = (B & C) | (~B & D); g = i; }
                    else if (i < 32) { F = (D & B) | (~D & C); g = (5 * i + 1) % 16; }
                    else if (i < 48) { F = B ^ C ^ D; g = (3 * i + 5) % 16; }
                    else { F = C ^ (B | ~D); g = (7 * i) % 16; }
                    F = (F + A + MD5_K[i] + M[g]) >>> 0;
                    A = D; D = C; C = B;
                    B = (B + rotl32(F, MD5_S[i])) >>> 0;
                }
                a0 = (a0 + A) >>> 0; b0 = (b0 + B) >>> 0; c0 = (c0 + C) >>> 0; d0 = (d0 + D) >>> 0;
            }
            const out = [];
            for (const w of [a0, b0, c0, d0]) {
                out.push(w & 255, (w >>> 8) & 255, (w >>> 16) & 255, (w >>> 24) & 255);
            }
            return out;
        };
        // Big-endian padding shared by SHA-1 and SHA-2.
        const padBigEndian = (bytes) => {
            const bitLen = bytes.length * 8;
            const padded = bytes.slice();
            padded.push(128);
            while (padded.length % 64 !== 56) padded.push(0);
            const hi = Math.floor(bitLen / 4294967296);
            const lo = bitLen >>> 0;
            padded.push((hi >>> 24) & 255, (hi >>> 16) & 255, (hi >>> 8) & 255, hi & 255,
                (lo >>> 24) & 255, (lo >>> 16) & 255, (lo >>> 8) & 255, lo & 255);
            return padded;
        };
        const blockWordsBE = (padded, off) => {
            const M = new Array(16);
            for (let i = 0; i < 16; i++) {
                M[i] = ((padded[off + i * 4] << 24) | (padded[off + i * 4 + 1] << 16) | (padded[off + i * 4 + 2] << 8) | padded[off + i * 4 + 3]) >>> 0;
            }
            return M;
        };
        // SHA-1 (RFC 3174).
        const sha1Digest = (bytes) => {
            const padded = padBigEndian(bytes);
            let h0 = 0x67452301, h1 = 0xefcdab89, h2 = 0x98badcfe, h3 = 0x10325476, h4 = 0xc3d2e1f0;
            const w = new Array(80);
            for (let off = 0; off < padded.length; off += 64) {
                const M = blockWordsBE(padded, off);
                for (let i = 0; i < 16; i++) w[i] = M[i];
                for (let i = 16; i < 80; i++) w[i] = rotl32(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
                let a = h0, b = h1, c = h2, d = h3, e = h4;
                for (let i = 0; i < 80; i++) {
                    let f, k;
                    if (i < 20) { f = (b & c) | (~b & d); k = 0x5a827999; }
                    else if (i < 40) { f = b ^ c ^ d; k = 0x6ed9eba1; }
                    else if (i < 60) { f = (b & c) | (b & d) | (c & d); k = 0x8f1bbcdc; }
                    else { f = b ^ c ^ d; k = 0xca62c1d6; }
                    const temp = (rotl32(a, 5) + f + e + k + w[i]) >>> 0;
                    e = d; d = c; c = rotl32(b, 30); b = a; a = temp;
                }
                h0 = (h0 + a) >>> 0; h1 = (h1 + b) >>> 0; h2 = (h2 + c) >>> 0; h3 = (h3 + d) >>> 0; h4 = (h4 + e) >>> 0;
            }
            const out = [];
            for (const v of [h0, h1, h2, h3, h4]) {
                out.push((v >>> 24) & 255, (v >>> 16) & 255, (v >>> 8) & 255, v & 255);
            }
            return out;
        };
        const SHA256_H = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
        ];
        const SHA256_K = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        const rotr32 = (x, n) => ((x >>> n) | (x << (32 - n))) >>> 0;
        // SHA-256 (FIPS 180-4).
        const sha256Digest = (bytes) => {
            const padded = padBigEndian(bytes);
            let [h0, h1, h2, h3, h4, h5, h6, h7] = SHA256_H;
            const w = new Array(64);
            for (let off = 0; off < padded.length; off += 64) {
                const M = blockWordsBE(padded, off);
                for (let i = 0; i < 16; i++) w[i] = M[i];
                for (let i = 16; i < 64; i++) {
                    const s0 = rotr32(w[i - 15], 7) ^ rotr32(w[i - 15], 18) ^ (w[i - 15] >>> 3);
                    const s1 = rotr32(w[i - 2], 17) ^ rotr32(w[i - 2], 19) ^ (w[i - 2] >>> 10);
                    w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
                }
                let [a, b, c, d, e, f, g, h] = [h0, h1, h2, h3, h4, h5, h6, h7];
                for (let i = 0; i < 64; i++) {
                    const S1 = rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25);
                    const ch = (e & f) ^ (~e & g);
                    const t1 = (h + S1 + ch + SHA256_K[i] + w[i]) >>> 0;
                    const S0 = rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22);
                    const maj = (a & b) ^ (a & c) ^ (b & c);
                    const t2 = (S0 + maj) >>> 0;
                    h = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0;
                }
                h0 = (h0 + a) >>> 0; h1 = (h1 + b) >>> 0; h2 = (h2 + c) >>> 0; h3 = (h3 + d) >>> 0;
                h4 = (h4 + e) >>> 0; h5 = (h5 + f) >>> 0; h6 = (h6 + g) >>> 0; h7 = (h7 + h) >>> 0;
            }
            const out = [];
            for (const v of [h0, h1, h2, h3, h4, h5, h6, h7]) {
                out.push((v >>> 24) & 255, (v >>> 16) & 255, (v >>> 8) & 255, v & 255);
            }
            return out;
        };
        const normalizeDigest = (algorithm) => {
            const name = String(algorithm).toLowerCase().replace(/[-_]/g, "");
            if (name === "md5" || name === "sha1" || name === "sha256") return name;
            const error = new Error(`Unknown digest: ${algorithm}`);
            error.code = "ERR_CRYPTO_UNKNOWN_DIGEST";
            throw error;
        };
        const digestBytes = (algorithm, bytes) => {
            if (algorithm === "md5") return md5Digest(bytes);
            if (algorithm === "sha1") return sha1Digest(bytes);
            return sha256Digest(bytes);
        };
        const digestLength = (algorithm) => (algorithm === "md5" ? 16 : algorithm === "sha1" ? 20 : 32);
        // HMAC over the digests above (RFC 2104); all three hashes share
        // the 64-byte block size.
        const hmacBytes = (algorithm, keyBytes, messageBytes) => {
            let key = keyBytes.slice();
            if (key.length > 64) key = digestBytes(algorithm, key);
            while (key.length < 64) key.push(0);
            const inner = key.map((b) => b ^ 0x36).concat(messageBytes);
            const outer = key.map((b) => b ^ 0x5c).concat(digestBytes(algorithm, inner));
            return digestBytes(algorithm, outer);
        };
        // PBKDF2-HMAC (RFC 2898).
        const pbkdf2SyncBytes = (password, salt, iterations, keylen, algorithm) => {
            const count = Number(iterations);
            const length = Number(keylen);
            if (!Number.isInteger(count) || count < 1) throw new TypeError("iterations must be a positive integer");
            if (!Number.isInteger(length) || length < 1) throw new TypeError("keylen must be a positive integer");
            const passBytes = cryptoBytes(password);
            const saltBytes = cryptoBytes(salt);
            const hashLen = digestLength(algorithm);
            const blocks = Math.ceil(length / hashLen);
            const out = [];
            for (let block = 1; block <= blocks; block++) {
                const counter = [(block >>> 24) & 255, (block >>> 16) & 255, (block >>> 8) & 255, block & 255];
                let u = hmacBytes(algorithm, passBytes, saltBytes.concat(counter));
                const acc = u.slice();
                for (let i = 1; i < count; i++) {
                    u = hmacBytes(algorithm, passBytes, u);
                    for (let j = 0; j < acc.length; j++) acc[j] ^= u[j];
                }
                out.push(...acc);
            }
            return out.slice(0, length);
        };
        const outputDigest = (bytes, encoding) => {
            if (encoding === undefined) return Buffer.from(bytes);
            return Buffer.from(bytes).toString(encoding);
        };
        function Hash(algorithm) {
            if (!(this instanceof Hash)) return new Hash(algorithm);
            this.algorithm = normalizeDigest(algorithm);
            this.chunks = [];
            this.finalized = false;
        }
        Hash.prototype.update = function (data, encoding) {
            if (this.finalized) throw new Error("Digest already called");
            this.chunks.push(...cryptoBytes(data, encoding));
            return this;
        };
        Hash.prototype.digest = function (encoding) {
            if (this.finalized) throw new Error("Digest already called");
            this.finalized = true;
            return outputDigest(digestBytes(this.algorithm, this.chunks), encoding);
        };
        Hash.prototype.copy = function () {
            const clone = new Hash(this.algorithm);
            clone.chunks = this.chunks.slice();
            return clone;
        };
        function Hmac(algorithm, key, keyEncoding) {
            if (!(this instanceof Hmac)) return new Hmac(algorithm, key, keyEncoding);
            this.algorithm = normalizeDigest(algorithm);
            this.keyBytes = cryptoBytes(key, keyEncoding);
            this.chunks = [];
            this.finalized = false;
        }
        Hmac.prototype.update = Hash.prototype.update;
        Hmac.prototype.digest = function (encoding) {
            if (this.finalized) throw new Error("Digest already called");
            this.finalized = true;
            return outputDigest(hmacBytes(this.algorithm, this.keyBytes, this.chunks), encoding);
        };
        return {
            createHash: (algorithm) => new Hash(algorithm),
            createHmac: (algorithm, key, keyEncoding) => new Hmac(algorithm, key, keyEncoding),
            Hash,
            Hmac,
            pbkdf2Sync: (password, salt, iterations, keylen, digest) => {
                const algorithm = normalizeDigest(digest === undefined ? "sha1" : digest);
                return Buffer.from(pbkdf2SyncBytes(password, salt, iterations, keylen, algorithm));
            },
            pbkdf2: (password, salt, iterations, keylen, digest, callback) => {
                if (typeof digest === "function") {
                    callback = digest;
                    digest = undefined;
                }
                if (typeof callback !== "function") throw new TypeError("callback must be a function");
                queueMicrotask(() => {
                    try {
                        const algorithm = normalizeDigest(digest === undefined ? "sha1" : digest);
                        callback(null, Buffer.from(pbkdf2SyncBytes(password, salt, iterations, keylen, algorithm)));
                    } catch (error) {
                        callback(error);
                    }
                });
            },
            getHashes: () => ["md5", "sha1", "sha256"],
            // Real entropy (issue #155): `__strake_random_bytes` fills from
            // the OS CSPRNG — no JS-side math — so `uuid` seeding and
            // session IDs match Node. The callback form settles on the
            // microtask queue, like every other async stand-in here.
            randomBytes(size, callback) {
                if (typeof size !== "number") {
                    const error = new TypeError(`The "size" argument must be of type number. Received ${size === null ? "null" : typeof size}`);
                    error.code = "ERR_INVALID_ARG_TYPE";
                    throw error;
                }
                if (!Number.isInteger(size) || size < 0 || size > 2147483647) {
                    const error = new RangeError(`The value of "size" is out of range. It must be >= 0 and <= 2147483647. Received ${size}`);
                    error.code = "ERR_OUT_OF_RANGE";
                    throw error;
                }
                const bytes = Buffer.from(__strake_random_bytes(size));
                if (callback === undefined) return bytes;
                if (typeof callback !== "function") {
                    const error = new TypeError(`The "callback" argument must be of type function. Received ${typeof callback}`);
                    error.code = "ERR_INVALID_ARG_TYPE";
                    throw error;
                }
                queueMicrotask(() => callback(null, bytes));
                return undefined;
            },
            randomUUID() {
                const bytes = [...__strake_random_bytes(16)];
                bytes[6] = (bytes[6] & 15) | 64;
                bytes[8] = (bytes[8] & 63) | 128;
                const hex = hexOfBytes(bytes);
                return hex.slice(0, 8) + "-" + hex.slice(8, 12) + "-" + hex.slice(12, 16) + "-" + hex.slice(16, 20) + "-" + hex.slice(20);
            },
            createCipher: (...args) => unavailableCrypto("createCipher"),
            createDecipher: (...args) => unavailableCrypto("createDecipher"),
            createCipheriv: (...args) => unavailableCrypto("createCipheriv"),
            createDecipheriv: (...args) => unavailableCrypto("createDecipheriv"),
            publicEncrypt: (...args) => unavailableCrypto("publicEncrypt"),
            privateDecrypt: (...args) => unavailableCrypto("privateDecrypt"),
            privateEncrypt: (...args) => unavailableCrypto("privateEncrypt"),
            publicDecrypt: (...args) => unavailableCrypto("publicDecrypt"),
            createSign: (...args) => unavailableCrypto("createSign"),
            createVerify: (...args) => unavailableCrypto("createVerify"),
            generateKeyPair: (...args) => unavailableCrypto("generateKeyPair"),
            generateKeyPairSync: (...args) => unavailableCrypto("generateKeyPairSync"),
            scrypt: (...args) => unavailableCrypto("scrypt"),
            scryptSync: (...args) => unavailableCrypto("scryptSync"),
            createDiffieHellman: (...args) => unavailableCrypto("createDiffieHellman"),
            createECDH: (...args) => unavailableCrypto("createECDH"),
        };
    })();
    // `os` host facts (issue #155): Joplin's updater, Sentry context, and
    // `human-signals` read these at import time. Metrics come from
    // `info.os` (Rust `std` sources: Linux `/proc`, env, `temp_dir`,
    // parallelism). Anything `std` cannot see keeps a marked fallback:
    // unknown numerics are `0`, unknown strings end in `-strake` (except
    // `hostname`, where `localhost` is the conventional default).
    const osModule = (() => {
        const host = info.os && typeof info.os === "object" ? info.os : {};
        const present = (value) => value !== undefined && value !== null;
        const text = (value, fallback) => (present(value) && String(value) !== "" ? String(value) : fallback);
        const num = (value) => (typeof value === "number" && value >= 0 ? value : 0);
        const platform = info.platform || "linux";
        const isMac = platform === "darwin";
        // POSIX signal numbers (issue #155): `human-signals` probes
        // `signals[name] !== undefined`, so every standard name is covered;
        // the few numbers that differ per kernel branch on `isMac`, like the
        // `constants` module's platform-varying `O_*` flags.
        const signals = {
            SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGILL: 4, SIGTRAP: 5, SIGABRT: 6, SIGIOT: 6,
            SIGBUS: isMac ? 10 : 7, SIGFPE: 8, SIGKILL: 9, SIGUSR1: isMac ? 30 : 10,
            SIGSEGV: 11, SIGUSR2: isMac ? 31 : 12, SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15,
            SIGSTKFLT: isMac ? undefined : 16, SIGCHLD: isMac ? 20 : 17,
            SIGCONT: isMac ? 19 : 18, SIGSTOP: isMac ? 17 : 19, SIGTSTP: isMac ? 18 : 20,
            SIGTTIN: 21, SIGTTOU: 22, SIGURG: isMac ? 16 : 23, SIGXCPU: 24, SIGXFSZ: 25,
            SIGVTALRM: 26, SIGPROF: 27, SIGWINCH: 28, SIGIO: isMac ? 23 : 29,
            SIGINFO: isMac ? 29 : undefined, SIGPWR: isMac ? undefined : 30,
            SIGSYS: isMac ? 12 : 31,
        };
        const cpus = Array.isArray(host.cpus) && host.cpus.length > 0
            ? host.cpus
            : [{ model: "", speed: 0 }];
        return {
            platform: () => platform,
            arch: () => text(host.arch, ""),
            release: () => text(host.release, "0.0.0-strake"),
            hostname: () => text(host.hostname, "localhost"),
            homedir: () => text(host.homedir, "/"),
            tmpdir: () => text(host.tmpdir, "/tmp"),
            EOL: platform === "win32" ? "\r\n" : "\n",
            uptime: () => num(host.uptime),
            totalmem: () => num(host.totalmem),
            freemem: () => num(host.freemem),
            cpus: () => cpus.map((cpu) => {
                const times = cpu && typeof cpu.times === "object" && cpu.times !== null ? cpu.times : {};
                return {
                    model: text(cpu && cpu.model, ""),
                    speed: num(cpu && cpu.speed),
                    times: {
                        user: num(times.user),
                        nice: num(times.nice),
                        sys: num(times.sys),
                        idle: num(times.idle),
                        irq: num(times.irq),
                    },
                };
            }),
            constants: { signals },
        };
    })();
    // `zlib` (issue #155): Joplin's updater needs real `gzipSync` /
    // `gunzipSync`, transports use the stream factories. DEFLATE runs in
    // the `flate2` natives; streams buffer input and transform once at
    // `end`, matching how the evidenced callers consume them (`write`
    // chunks, `end`, read `data`). Option bags (`level`, `flush`) are
    // accepted and use the defaults, like Node's.
    const zlibModule = (() => {
        const sync = (direction, wrapper, data) => {
            const input = typeof data === "string" ? Buffer.from(data, "utf8") : Buffer.from(data);
            const out = direction === "deflate"
                ? __strake_zlib_deflate(input, wrapper)
                : __strake_zlib_inflate(input, wrapper);
            return Buffer.from(out);
        };
        const zlibStream = (direction, wrapper, options) => {
            const state = { input: [] };
            return new streamModule.Transform({
                transform(chunk, encoding, callback) {
                    state.input.push(
                        typeof chunk === "string" ? Buffer.from(chunk, encoding || "utf8") : Buffer.from(chunk)
                    );
                    callback();
                },
                flush(callback) {
                    let total = 0;
                    for (const part of state.input) total += part.length;
                    const flat = new Buffer(total);
                    let off = 0;
                    for (const part of state.input) {
                        flat.set(part, off);
                        off += part.length;
                    }
                    state.input = [];
                    let out;
                    try {
                        out = direction === "deflate"
                            ? __strake_zlib_deflate(flat, wrapper)
                            : __strake_zlib_inflate(flat, wrapper);
                    } catch (error) {
                        callback(error);
                        return;
                    }
                    this.push(Buffer.from(out));
                    callback();
                },
            });
        };
        return {
            gzipSync: (data) => sync("deflate", "gzip", data),
            gunzipSync: (data) => sync("inflate", "gzip", data),
            deflateSync: (data) => sync("deflate", "zlib", data),
            inflateSync: (data) => sync("inflate", "zlib", data),
            deflateRawSync: (data) => sync("deflate", "raw", data),
            inflateRawSync: (data) => sync("inflate", "raw", data),
            createGzip: (options) => zlibStream("deflate", "gzip", options),
            createGunzip: (options) => zlibStream("inflate", "gzip", options),
            createDeflate: (options) => zlibStream("deflate", "zlib", options),
            createInflate: (options) => zlibStream("inflate", "zlib", options),
            createDeflateRaw: (options) => zlibStream("deflate", "raw", options),
            createInflateRaw: (options) => zlibStream("inflate", "raw", options),
            Z_SYNC_FLUSH: 2,
        };
    })();
    // `http`/`https` client (issue #155): Joplin's updater, `got`, Sentry,
    // and `form-data` transports call `request` with keep-alive `Agent`s —
    // including `agentkeepalive` subclasses, so `Agent` is a plain-function
    // constructor (subclassable, `.apply`-wrappable) with a real
    // `prototype.addRequest`. Transfers run in the `reqwest` native at
    // `Writable._final` time and surface on microtasks, keeping async
    // ordering. Pooling options are accepted and stored; reuse is the
    // transport's business. In-flight `abort()` cannot preempt the blocking
    // native (it errors the request instead). `listen()` stays
    // coded-unavailable: serving needs the threaded bridge (follow-up).
    const httpClientShapes = (() => {
        function ClientRequest(url, options, callback) {
            if (!(this instanceof ClientRequest)) return new ClientRequest(url, options, callback);
            streamModule.Writable.call(this);
            const parsed = new URL(url);
            this.protocol = parsed.protocol;
            this.host = parsed.hostname;
            this.port = parsed.port;
            this.path = parsed.pathname + parsed.search;
            this.method = String((options && options.method) || "GET").toUpperCase();
            this._httpUrl = url;
            this._httpHeaders = {};
            const initial = (options && options.headers) || {};
            for (const name of Object.keys(initial)) this.setHeader(name, initial[name]);
            this._httpTimeout = options && typeof options.timeout === "number" ? options.timeout : 0;
            this._httpAgent = options ? options.agent : undefined;
            this._httpChunks = [];
            this._httpAborted = false;
            if (typeof callback === "function") this.once("response", callback);
        }
        ClientRequest.prototype = Object.create(streamModule.Writable.prototype);
        ClientRequest.prototype.constructor = ClientRequest;
        ClientRequest.prototype.setHeader = function (name, value) {
            this._httpHeaders[String(name).toLowerCase()] = { name: String(name), value };
        };
        ClientRequest.prototype.getHeader = function (name) {
            const found = this._httpHeaders[String(name).toLowerCase()];
            return found === undefined ? undefined : found.value;
        };
        ClientRequest.prototype.removeHeader = function (name) {
            delete this._httpHeaders[String(name).toLowerCase()];
        };
        ClientRequest.prototype.getHeaders = function () {
            const out = {};
            for (const key of Object.keys(this._httpHeaders)) {
                out[this._httpHeaders[key].name] = this._httpHeaders[key].value;
            }
            return out;
        };
        ClientRequest.prototype.hasHeader = function (name) {
            return this._httpHeaders[String(name).toLowerCase()] !== undefined;
        };
        ClientRequest.prototype._write = function (chunk, encoding, callback) {
            this._httpChunks.push(
                typeof chunk === "string" ? Buffer.from(chunk, encoding || "utf8") : Buffer.from(chunk)
            );
            callback();
        };
        ClientRequest.prototype._final = function (callback) {
            const self = this;
            const fail = (error) => queueMicrotask(() => self.emit("error", error));
            if (self._httpAborted) {
                const aborted = new Error("socket hang up");
                aborted.code = "ECONNRESET";
                fail(aborted);
                callback();
                return;
            }
            let total = 0;
            for (const part of self._httpChunks) total += part.length;
            const flat = new Buffer(total);
            let off = 0;
            for (const part of self._httpChunks) {
                flat.set(part, off);
                off += part.length;
            }
            self._httpChunks = [];
            const pairs = [];
            for (const key of Object.keys(self._httpHeaders)) {
                const header = self._httpHeaders[key];
                if (Array.isArray(header.value)) {
                    for (const item of header.value) pairs.push([header.name, String(item)]);
                } else if (header.value !== undefined) {
                    pairs.push([header.name, String(header.value)]);
                }
            }
            let result;
            try {
                result = __strake_http_fetch(
                    self._httpUrl, self.method, JSON.stringify(pairs),
                    total > 0 ? flat : null, self._httpTimeout
                );
            } catch (error) {
                fail(error);
                callback();
                return;
            }
            const res = new IncomingMessage(result);
            callback();
            queueMicrotask(() => self.emit("response", res));
        };
        ClientRequest.prototype.abort = function () {
            if (this._httpAborted) return;
            this._httpAborted = true;
            const self = this;
            const aborted = new Error("socket hang up");
            aborted.code = "ECONNRESET";
            queueMicrotask(() => {
                self.emit("abort");
                self.emit("error", aborted);
            });
        };
        ClientRequest.prototype.destroy = function (error) {
            if (error !== undefined) {
                const self = this;
                queueMicrotask(() => self.emit("error", error));
            }
            return this;
        };
        ClientRequest.prototype.setTimeout = function (timeout, callback) {
            this._httpTimeout = Number(timeout) || 0;
            if (typeof callback === "function") this.once("timeout", callback);
            return this;
        };
        function IncomingMessage(result) {
            if (!(this instanceof IncomingMessage)) return new IncomingMessage(result);
            streamModule.Readable.call(this);
            this.statusCode = result.status;
            this.statusMessage = result.statusMessage;
            this.headers = {};
            this.rawHeaders = [];
            for (const pair of JSON.parse(result.headersJson)) {
                const key = String(pair[0]).toLowerCase();
                this.rawHeaders.push(String(pair[0]), String(pair[1]));
                if (key === "set-cookie") {
                    if (this.headers[key] === undefined) this.headers[key] = [];
                    this.headers[key].push(String(pair[1]));
                } else if (this.headers[key] === undefined) {
                    this.headers[key] = String(pair[1]);
                } else {
                    this.headers[key] += ", " + String(pair[1]);
                }
            }
            this.httpVersion = "1.1";
            this.httpVersionMajor = 1;
            this.httpVersionMinor = 1;
            this.complete = false;
            this.aborted = false;
            this.push(Buffer.from(result.body));
            this.push(null);
            const self = this;
            this.once("end", () => {
                self.complete = true;
            });
        }
        IncomingMessage.prototype = Object.create(streamModule.Readable.prototype);
        IncomingMessage.prototype.constructor = IncomingMessage;
        IncomingMessage.prototype.destroy = function (error) {
            if (error !== undefined) {
                const self = this;
                queueMicrotask(() => self.emit("error", error));
            }
            return this;
        };
        function Server(requestListener) {
            if (!(this instanceof Server)) return new Server(requestListener);
            eventsModule.EventEmitter.call(this);
            if (typeof requestListener === "function") this.on("request", requestListener);
            this.listening = false;
            this.timeout = 0;
        }
        Server.prototype = Object.create(eventsModule.EventEmitter.prototype);
        Server.prototype.constructor = Server;
        Server.prototype.listen = function () {
            const error = new Error("listen is not available in this host (threaded serving bridge; see issue #155)");
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = "listen";
            throw error;
        };
        Server.prototype.close = function (callback) {
            if (typeof callback === "function") queueMicrotask(() => callback());
            return this;
        };
        Server.prototype.setTimeout = function (timeout, callback) {
            this.timeout = Number(timeout) || 0;
            if (typeof callback === "function") this.on("timeout", callback);
            return this;
        };
        return { ClientRequest, IncomingMessage, Server };
    })();
    const makeHttpModule = (agentName, defaultPort, defaultProtocol) => {
        function Agent(options) {
            if (!(this instanceof Agent)) return new Agent(options);
            eventsModule.EventEmitter.call(this);
            const opts = options || {};
            this.options = Object.assign({}, opts);
            this.keepAlive = !!opts.keepAlive;
            this.maxSockets = opts.maxSockets === undefined ? Infinity : Number(opts.maxSockets);
            this.maxFreeSockets = opts.maxFreeSockets === undefined ? 256 : Number(opts.maxFreeSockets);
            this.timeout = opts.timeout === undefined ? 0 : Number(opts.timeout);
            this.freeSockets = {};
            this.sockets = {};
            this.requests = {};
            this.defaultPort = defaultPort;
            this.protocol = defaultProtocol;
        }
        Object.defineProperty(Agent, "name", { value: agentName });
        Agent.prototype = Object.create(eventsModule.EventEmitter.prototype);
        Agent.prototype.constructor = Agent;
        // Pooling is the transport's business; the default records the
        // request so subclass save-and-override (`agentkeepalive`) works.
        Agent.prototype.addRequest = function (req, options) {
            req._httpAgentOptions = options;
        };
        Agent.prototype.destroy = function () {
            this.freeSockets = {};
            this.sockets = {};
        };
        const toUrl = (requestUrl, options) => {
            const parsed = new URL(requestUrl === null ? defaultProtocol + "//localhost/" : requestUrl);
            let hostname = options.hostname;
            let port = options.port;
            if (hostname === undefined && typeof options.host === "string" && options.host !== "") {
                const divider = options.host.lastIndexOf(":");
                if (divider !== -1 && /^[0-9]+$/.test(options.host.slice(divider + 1))) {
                    hostname = options.host.slice(0, divider);
                    if (port === undefined) port = options.host.slice(divider + 1);
                } else {
                    hostname = options.host;
                }
            }
            if (hostname === undefined) hostname = parsed.hostname || "localhost";
            if (port === undefined) port = parsed.port !== "" ? parsed.port : "";
            const path = options.path !== undefined ? options.path : (parsed.pathname || "/") + parsed.search;
            const protocol = options.protocol !== undefined ? options.protocol : parsed.protocol;
            return protocol + "//" + hostname + (port === "" ? "" : ":" + port) + path;
        };
        const parseRequestArgs = (urlOrOptions, optionsOrCallback, maybeCallback) => {
            let requestUrl = null;
            let options = {};
            let callback;
            if (typeof urlOrOptions === "string" || urlOrOptions instanceof URL) {
                requestUrl = String(urlOrOptions);
                if (typeof optionsOrCallback === "function") callback = optionsOrCallback;
                else {
                    options = optionsOrCallback || {};
                    callback = maybeCallback;
                }
            } else {
                options = urlOrOptions || {};
                if (typeof optionsOrCallback === "function") callback = optionsOrCallback;
            }
            return { requestUrl, options, callback };
        };
        const request = (urlOrOptions, optionsOrCallback, maybeCallback) => {
            const parsed = parseRequestArgs(urlOrOptions, optionsOrCallback, maybeCallback);
            return new httpClientShapes.ClientRequest(toUrl(parsed.requestUrl, parsed.options), parsed.options, parsed.callback);
        };
        const get = (urlOrOptions, optionsOrCallback, maybeCallback) => {
            const req = request(urlOrOptions, optionsOrCallback, maybeCallback);
            req.end();
            return req;
        };
        return {
            request,
            get,
            Agent,
            ClientRequest: httpClientShapes.ClientRequest,
            IncomingMessage: httpClientShapes.IncomingMessage,
            Server: httpClientShapes.Server,
            globalAgent: new Agent(),
            createServer: (options, listener) => {
                if (typeof options === "function") listener = options;
                return new httpClientShapes.Server(listener);
            },
            defaultMaxSockets: Infinity,
        };
    };
    const httpModule = makeHttpModule("HttpAgent", 80, "http:");
    const httpsModule = makeHttpModule("HttpsAgent", 443, "https:");
    // `net` (issue #155): Joplin's port probe needs real connect-vs-refused
    // truth, and `agentkeepalive` aliases `createConnection` at import.
    // `isIP` is a real parser; `Socket.connect` really connects (blocking
    // native, outcome on a microtask) and holds the handle for `end` /
    // `destroy`. Byte transfer stays follow-up work — `_write` reports it
    // coded instead of black-holing.
    const netModule = (() => {
        const isIPv4 = (value) => {
            if (typeof value !== "string") return false;
            const parts = value.split(".");
            if (parts.length !== 4) return false;
            for (const part of parts) {
                if (!/^(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])$/.test(part)) return false;
            }
            return true;
        };
        const isIPv6 = (value) => {
            if (typeof value !== "string" || value === "" || value.includes("%")) return false;
            const halves = value.split("::");
            if (halves.length > 2) return false;
            const compressed = halves.length === 2;
            const groups = [];
            for (const half of halves) {
                if (half !== "") groups.push(...half.split(":"));
            }
            let count = 0;
            for (let i = 0; i < groups.length; i++) {
                const group = groups[i];
                if (group.includes(".")) {
                    if (i !== groups.length - 1 || !isIPv4(group)) return false;
                    count += 2;
                } else {
                    if (!/^[0-9a-fA-F]{1,4}$/.test(group)) return false;
                    count += 1;
                }
            }
            return compressed ? count < 8 : count === 8;
        };
        const unavailablePipe = (verb) => {
            const error = new Error(`${verb} is not available in this host (duplex bridge; see issue #155)`);
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = verb;
            return error;
        };
        function Socket(options) {
            if (!(this instanceof Socket)) return new Socket(options);
            streamModule.Duplex.call(this, options);
            const opts = options || {};
            this._netHandle = null;
            this._netTimeout = typeof opts.timeout === "number" ? opts.timeout : 0;
            this._netNoDelay = true;
            this._netKeepAlive = false;
            this._netKeepAliveDelay = 0;
            this.connecting = false;
            this.destroyed = false;
            this._unrefed = false;
        }
        Socket.prototype = Object.create(streamModule.Duplex.prototype);
        Socket.prototype.constructor = Socket;
        Socket.prototype.connect = function (portOrOptions, hostOrCallback, maybeCallback) {
            const self = this;
            let options;
            let callback;
            if (typeof portOrOptions === "object" && portOrOptions !== null) {
                options = portOrOptions;
                callback = typeof hostOrCallback === "function" ? hostOrCallback : undefined;
            } else if (typeof portOrOptions === "number" || /^\d+$/.test(String(portOrOptions))) {
                options = { port: Number(portOrOptions) };
                if (typeof hostOrCallback === "string") {
                    options.host = hostOrCallback;
                    callback = maybeCallback;
                } else {
                    callback = hostOrCallback;
                }
            } else {
                throw unavailablePipe("connect");
            }
            if (options.path !== undefined) throw unavailablePipe("connect");
            const timeout = options.timeout !== undefined ? options.timeout : self._netTimeout;
            self.connecting = true;
            let handle;
            try {
                handle = __strake_net_connect(
                    options.host || options.hostname || "localhost",
                    options.port === undefined ? 80 : Number(options.port),
                    typeof timeout === "number" && timeout > 0 ? timeout : 0
                );
            } catch (error) {
                self.connecting = false;
                queueMicrotask(() => self.emit("error", error));
                return self;
            }
            self._netHandle = handle;
            self.connecting = false;
            queueMicrotask(() => {
                self.emit("connect");
                if (typeof callback === "function") callback();
            });
            return self;
        };
        Socket.prototype._write = function (chunk, encoding, callback) {
            callback(unavailablePipe("write"));
        };
        Socket.prototype._final = function (callback) {
            this._closeHandle();
            callback();
        };
        Socket.prototype._closeHandle = function () {
            if (this._netHandle !== null && this._netHandle !== undefined) {
                try {
                    __strake_net_close(this._netHandle);
                } catch (e) {
                    // Idempotent: the handle may already be gone.
                }
                this._netHandle = null;
            }
        };
        Socket.prototype.destroy = function (error) {
            const self = this;
            this._closeHandle();
            this.destroyed = true;
            queueMicrotask(() => {
                if (error !== undefined) self.emit("error", error);
                self.emit("close", error !== undefined);
            });
            return this;
        };
        Socket.prototype.setTimeout = function (timeout, callback) {
            // Stored for `connect` (no inactivity pump exists yet — the
            // `timeout` event fires with the duplex bridge).
            this._netTimeout = Number(timeout) || 0;
            if (typeof callback === "function") this.once("timeout", callback);
            return this;
        };
        Socket.prototype.setNoDelay = function (noDelay) {
            this._netNoDelay = noDelay === undefined ? true : !!noDelay;
            return this;
        };
        Socket.prototype.setKeepAlive = function (enable, initialDelay) {
            this._netKeepAlive = !!enable;
            this._netKeepAliveDelay = initialDelay === undefined ? 0 : Number(initialDelay);
            return this;
        };
        Socket.prototype.ref = function () {
            this._unrefed = false;
            return this;
        };
        Socket.prototype.unref = function () {
            this._unrefed = true;
            return this;
        };
        function Server(requestListener) {
            if (!(this instanceof Server)) return new Server(requestListener);
            eventsModule.EventEmitter.call(this);
            if (typeof requestListener === "function") this.on("connection", requestListener);
            this.listening = false;
            this.timeout = 0;
        }
        Server.prototype = Object.create(eventsModule.EventEmitter.prototype);
        Server.prototype.constructor = Server;
        Server.prototype.listen = function () {
            const error = new Error("listen is not available in this host (threaded serving bridge; see issue #155)");
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = "listen";
            throw error;
        };
        Server.prototype.close = function (callback) {
            if (typeof callback === "function") queueMicrotask(() => callback());
            return this;
        };
        const connect = (...args) => new Socket().connect(...args);
        return {
            Socket,
            Server,
            createServer: (options, listener) => {
                if (typeof options === "function") listener = options;
                return new Server(listener);
            },
            connect,
            createConnection: connect,
            isIP: (value) => (isIPv4(value) ? 4 : isIPv6(value) ? 6 : 0),
            isIPv4,
            isIPv6,
        };
    })();
    // `tls` (issue #155): a Node-compat shim calls `createSecureContext()`
    // at import and wraps it, so the factory is real (options stored — no
    // crypto until a socket uses it). `connect()` needs the duplex bridge,
    // coded-unavailable like `net` writes.
    const tlsModule = (() => {
        const createSecureContext = (options) => ({ options: Object.assign({}, options) });
        const connect = () => {
            const error = new Error("tls.connect is not available in this host (duplex bridge; see issue #155)");
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = "connect";
            throw error;
        };
        return { createSecureContext, connect };
    })();
    // `domain` (issue #155): Sentry reads `domain.active` and falls back to
    // `domain.create()` + `bind()` as its async-context carrier. `run`
    // executes synchronously between `enter`/`exit`, so `active` is real
    // inside `run`; async continuation tracking is out of scope (no
    // `async_hooks` — same standing as every other shim here).
    const domainModule = (() => {
        let activeDomain = null;
        function Domain() {
            if (!(this instanceof Domain)) return new Domain();
            eventsModule.EventEmitter.call(this);
            this.members = [];
        }
        Domain.prototype = Object.create(eventsModule.EventEmitter.prototype);
        Domain.prototype.constructor = Domain;
        Domain.prototype.enter = function () {
            activeDomain = this;
        };
        Domain.prototype.exit = function () {
            if (activeDomain === this) activeDomain = null;
        };
        Domain.prototype.run = function (fn, ...args) {
            this.enter();
            try {
                const result = fn(...args);
                this.exit();
                return result;
            } catch (error) {
                this.exit();
                if (this.listeners("error").length > 0) {
                    this.emit("error", error);
                    return undefined;
                }
                throw error;
            }
        };
        Domain.prototype.bind = function (fn) {
            const self = this;
            return function (...args) {
                return self.run(() => fn(...args));
            };
        };
        Domain.prototype.intercept = function (fn) {
            const self = this;
            return function (error, ...args) {
                if (error) {
                    self.emit("error", error);
                    return undefined;
                }
                return self.run(() => fn(...args));
            };
        };
        return {
            create: () => new Domain(),
            get active() {
                return activeDomain;
            },
        };
    })();
    // `async_hooks.AsyncLocalStorage` (issue #155): Sentry's async-context
    // strategy runs hubs via `run(store, fn)` and reads `getStore()`.
    // Synchronous propagation is real (nesting restores, throws restore);
    // cross-microtask tracking needs true async resources (out of scope —
    // same standing as `domain` above).
    const asyncHooksModule = (() => {
        function AsyncLocalStorage() {
            if (!(this instanceof AsyncLocalStorage)) return new AsyncLocalStorage();
            this._store = undefined;
        }
        AsyncLocalStorage.prototype.getStore = function () {
            return this._store;
        };
        AsyncLocalStorage.prototype.run = function (store, fn, ...args) {
            const previous = this._store;
            this._store = store;
            try {
                const result = fn(...args);
                this._store = previous;
                return result;
            } catch (error) {
                this._store = previous;
                throw error;
            }
        };
        AsyncLocalStorage.prototype.enterWith = function (store) {
            this._store = store;
        };
        return { AsyncLocalStorage };
    })();
    // `dgram` surface (issue #155): Joplin's shim registry requires it at
    // load but only touches it through a lazy accessor. Socket creation
    // stays coded-unavailable: silent no-op sockets would drop sync
    // traffic without a trace.
    const dgramModule = (() => {
        const unavailable = (name) => {
            const error = new Error(`${name} is not available in this host (no datagram transport; see issue #155)`);
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = name;
            throw error;
        };
        return {
            createSocket: (...args) => unavailable("createSocket"),
        };
    })();
    // `dns` result-order state (issue #155): Joplin's CLI layer requires
    // the module at load and conditionally calls
    // `setDefaultResultOrder("ipv4first")`. The order flag is real
    // (validated like Node, `verbatim` default); actual resolution needs
    // a datagram transport and stays out of scope.
    const dnsModule = (() => {
        let defaultResultOrder = "verbatim";
        return {
            getDefaultResultOrder() {
                return defaultResultOrder;
            },
            setDefaultResultOrder(order) {
                if (order !== "ipv4first" && order !== "verbatim") {
                    const error = new TypeError(`The argument 'order' must be one of 'ipv4first' or 'verbatim'. Received '${order}'`);
                    error.code = "ERR_INVALID_ARG_VALUE";
                    throw error;
                }
                defaultResultOrder = order;
            },
        };
    })();
    // `http2` surface (issue #155): Joplin's sync stack requires it at
    // load and uses `constants` pseudo-headers plus `connect()` at request
    // time. Constants are real (RFC 7540); sessions stay
    // coded-unavailable (no HTTP/2 transport in this host).
    const http2Module = (() => {
        const unavailable = (name) => {
            const error = new Error(`${name} is not available in this host (no HTTP/2 transport; see issue #155)`);
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = name;
            throw error;
        };
        return {
            constants: {
                HTTP2_HEADER_STATUS: ":status",
                HTTP2_HEADER_METHOD: ":method",
                HTTP2_HEADER_PATH: ":path",
                HTTP2_HEADER_SCHEME: ":scheme",
                HTTP2_HEADER_AUTHORITY: ":authority",
            },
            connect: (...args) => unavailable("connect"),
            createServer: (...args) => unavailable("createServer"),
            createSecureServer: (...args) => unavailable("createSecureServer"),
        };
    })();
    // `tty` surface (issue #155): feature sniffers (`supports-color`,
    // `debug`) call `isatty` at import time. Headless boot has no terminal,
    // so `isatty` honestly reports `false` and raw mode stays unavailable.
    // Plain-function constructors (never classes): third-party shims wrap
    // stream constructors via `.apply`, which throws on classes.
    const ttyModule = (() => {
        function TtyReadStream(fd) {
            if (!(this instanceof TtyReadStream)) return new TtyReadStream(fd);
            eventsModule.EventEmitter.call(this);
            this.fd = fd === undefined ? 0 : Number(fd);
            this.isTTY = false;
            this.isRaw = false;
        }
        TtyReadStream.prototype = Object.create(streamModule.Stream.prototype);
        TtyReadStream.prototype.constructor = TtyReadStream;
        TtyReadStream.prototype.setRawMode = function (mode) {
            const error = new Error("setRawMode is not available in this host (no terminal; see issue #155)");
            error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
            error.syscall = "setRawMode";
            throw error;
        };
        function TtyWriteStream(fd) {
            if (!(this instanceof TtyWriteStream)) return new TtyWriteStream(fd);
            eventsModule.EventEmitter.call(this);
            this.fd = fd === undefined ? 1 : Number(fd);
            this.isTTY = false;
            this.columns = 80;
            this.rows = 24;
        }
        TtyWriteStream.prototype = Object.create(streamModule.Stream.prototype);
        TtyWriteStream.prototype.constructor = TtyWriteStream;
        TtyWriteStream.prototype.getWindowSize = function () {
            return [this.columns, this.rows];
        };
        TtyWriteStream.prototype.setRawMode = TtyReadStream.prototype.setRawMode;
        // Headless stdio writes route to the host console, keeping the
        // stdout/stderr distinction. The callback stays async (microtask).
        TtyWriteStream.prototype.write = function (chunk, encoding, callback) {
            if (typeof encoding === "function") callback = encoding;
            const text = typeof chunk === "string" ? chunk : String(chunk);
            if (this.fd === 2) console.error(text);
            else console.log(text);
            if (typeof callback === "function") queueMicrotask(() => callback());
            return true;
        };
        return {
            isatty: (fd) => false,
            ReadStream: TtyReadStream,
            WriteStream: TtyWriteStream,
        };
    })();
    // `process` stdio handles (issue #155): feature sniffers (`debug`,
    // `supports-color`) read `process.stderr.fd`/`isTTY` at import time.
    // Attached here — after `ttyModule` — so the handles are real
    // `tty.WriteStream`/`ReadStream` instances with the standard fds.
    processModule.stdin = new ttyModule.ReadStream(0);
    processModule.stdout = new ttyModule.WriteStream(1);
    processModule.stderr = new ttyModule.WriteStream(2);
    // Module table sits after every module IIFE above (issue #155: a table
    // entry referencing a later IIFE reads an uninitialized binding).
    globalThis.__strake_node_modules = {
        "node:path": pathModule,
        path: pathModule,
        "node:url": urlModule,
        url: urlModule,
        "node:process": processModule,
        process: processModule,
        "node:events": eventsModule,
        events: eventsModule,
        "node:constants": constantsModule,
        constants: constantsModule,
        "node:buffer": bufferModule,
        buffer: bufferModule,
        "node:string_decoder": stringDecoderModule,
        string_decoder: stringDecoderModule,
        "node:stream": streamModule,
        stream: streamModule,
        "node:util": utilModule,
        util: utilModule,
        "node:assert": assertModule,
        assert: assertModule,
        "node:child_process": childProcessModule,
        child_process: childProcessModule,
        "node:crypto": cryptoModule,
        crypto: cryptoModule,
        "node:tty": ttyModule,
        tty: ttyModule,
        "node:os": osModule,
        os: osModule,
        "node:zlib": zlibModule,
        zlib: zlibModule,
        "node:http": httpModule,
        http: httpModule,
        "node:https": httpsModule,
        https: httpsModule,
        "node:net": netModule,
        net: netModule,
        "node:tls": tlsModule,
        tls: tlsModule,
        "node:domain": domainModule,
        domain: domainModule,
        "node:async_hooks": asyncHooksModule,
        async_hooks: asyncHooksModule,
        "node:dgram": dgramModule,
        dgram: dgramModule,
        "node:dns": dnsModule,
        dns: dnsModule,
        "node:http2": http2Module,
        http2: http2Module,
        "node:timers": timersModule,
        timers: timersModule,
    };
    // Node's global `Buffer` (raw reads in Joplin startup use it bare).
    if (typeof globalThis.Buffer === "undefined") {
        globalThis.Buffer = bufferModule.Buffer;
    }
    if (typeof globalThis.process === "undefined") {
        globalThis.process = processModule;
    }
    if (typeof globalThis.performance === "undefined") {
        globalThis.performance = performanceModule;
    }
    // Node's `global` alias for the global object (`const { Promise } =
    // global` in `@electron/remote`): self-reference, exactly as in Node.
    if (typeof globalThis.global === "undefined") {
        globalThis.global = globalThis;
    }
    if (typeof globalThis.__dirname === "undefined") {
        globalThis.__dirname = info.appRoot || "/";
    }
})();
"#;

/// Real `node:fs` sync shell over the capability-gated `__strake_fs_*`
/// native primitives (issue #154). Evaluated in main-process installs only,
/// after the primitives are registered: renderer contexts never see `fs`
/// (real Electron defaults to no node integration in renderers).
///
/// Coverage is the sync subset `fs-extra`/`graceful-fs` needs at load plus
/// what Joplin startup calls: exists/access, recursive mkdir, read/write/
/// append, stat/lstat (`Stats`), readdir (names only — `withFileTypes` is
/// accepted and ignored), unlink/rename/copyFile, realpath, readlink,
/// utimes, `constants`, and working `ReadStream`/`WriteStream` classes with
/// `create*` factories. `fs/promises` wraps the sync subset on microtasks.
/// Watchers stay out of scope.
const NODE_FS_BOOTSTRAP_JS: &str = r#"
(function () {
    // Coded-error factory for the native primitives: `{ code, errno,
    // syscall, path }` on a real `Error`, so `catch (e) { e.code }` works
    // as in Node.
    globalThis.__strake_node_error = function (code, errno, syscall, path, message) {
        const error = new Error(message);
        error.code = code;
        error.errno = errno;
        error.syscall = syscall;
        error.path = path;
        return error;
    };
    const table = globalThis.__strake_node_modules;
    const events = table["node:events"];
    const stream = table["node:stream"];
    const bufferMod = table["node:buffer"];
    const Buffer = bufferMod.Buffer;
    const constants = table["node:constants"];
    const toPath = (value) => {
        if (typeof value === "string") return value;
        if (value instanceof Uint8Array) return Buffer.from(value).toString();
        if (value instanceof ArrayBuffer) return Buffer.from(value).toString();
        throw new TypeError("Path must be a string");
    };
    const toBytes = (data, encoding) => {
        if (typeof data === "string") return Buffer.from(data, encoding);
        return Buffer.from(data);
    };
    // `utimes` stamps (issue #155): Node takes seconds (numbers), date-time
    // strings, or `Date`s; the native takes seconds.
    const toUnixTime = (value) => {
        if (value instanceof Date) return value.getTime() / 1000;
        return Number(value);
    };
    class Stats {
        constructor(data) {
            this.size = data.size;
            this.mode = data.mode;
            this.mtimeMs = data.mtimeMs;
            this.atimeMs = data.atimeMs;
            this.ctimeMs = data.ctimeMs;
            this.birthtimeMs = data.birthtimeMs;
            this.mtime = new Date(data.mtimeMs);
            this.atime = new Date(data.atimeMs);
            this.ctime = new Date(data.ctimeMs);
            this.birthtime = new Date(data.birthtimeMs);
            this._file = data.isFile;
            this._dir = data.isDir;
            this._link = data.isSymlink;
        }
        isFile() { return this._file; }
        isDirectory() { return this._dir; }
        isSymbolicLink() { return this._link; }
        isBlockDevice() { return false; }
        isCharacterDevice() { return false; }
        isFIFO() { return false; }
        isSocket() { return false; }
    }
    // Plain-function constructors, deliberately NOT ES classes (issue
    // #155): `graceful-fs` wraps `ReadStream` in a function that delegates
    // via `.apply` and swaps the prototype's `open` — `.apply` on a class
    // constructor throws. Prototype chains still reach `stream.Stream`, so
    // `instanceof` checks hold both directions.
    function ReadStream(path, options) {
        if (!(this instanceof ReadStream)) return new ReadStream(path, options);
        events.EventEmitter.call(this);
        this.path = toPath(path);
        this.fd = null;
        this._ended = false;
        this._pumping = false;
        this._flowOnData = false;
        this._readPending = false;
        this._openFailed = false;
        this._openQueued = false;
        // Synchronous `open` call like real `fs.ReadStream` (and the
        // graceful-fs wrapper): the fd itself still lands on a microtask,
        // so streams created first complete first and an explicit later
        // `open()` cannot jump the queue.
        this.open();
    }
    ReadStream.prototype = Object.create(stream.Stream.prototype);
    ReadStream.prototype.constructor = ReadStream;
    ReadStream.prototype.open = function () {
        if (this.fd !== null || this._openFailed || this._ended) return;
        if (this._openQueued) return;
        this._openQueued = true;
        const self = this;
        queueMicrotask(() => {
            self._openQueued = false;
            self._doOpen();
        });
    };
    ReadStream.prototype._doOpen = function () {
        if (this.fd !== null || this._openFailed || this._ended) return;
        try {
            this.fd = __strake_fs_open(this.path, "r", 438);
        } catch (error) {
            this._openFailed = true;
            this.emit("error", error);
            return;
        }
        this.emit("open", this.fd);
        if (this._flowOnData || this._readPending) this.read();
    };
    // Pull one full pass: emit `data` per chunk, then `end`/`close`.
    // Wrapping openers (graceful-fs) call this after their own `open`.
    ReadStream.prototype.read = function () {
        if (this._ended || this._openFailed) return;
        if (this.fd === null) {
            // Open is deferred (Node opens async): park the pull so the
            // deferred opener pumps once the fd lands.
            this._readPending = true;
            this.open();
            return;
        }
        this._readPending = false;
        this._pump();
    };
    ReadStream.prototype._pump = function () {
        if (this._pumping || this._ended) return;
        this._pumping = true;
        try {
            for (;;) {
                const chunk = Buffer.alloc(64 * 1024);
                const n = __strake_fs_read_fd(this.fd, chunk, 0, chunk.length, null);
                if (n === 0) break;
                this.emit("data", chunk.slice(0, n));
            }
        } catch (error) {
            this._pumping = false;
            this.emit("error", error);
            return;
        }
        this._pumping = false;
        this._ended = true;
        this.emit("end");
        this.emit("close");
        try {
            __strake_fs_close(this.fd);
        } catch (e) {}
        this.fd = null;
    };
    ReadStream.prototype.close = function (callback) {
        this._ended = true;
        if (this.fd !== null) {
            try {
                __strake_fs_close(this.fd);
            } catch (e) {}
            this.fd = null;
        }
        this.emit("close");
        if (typeof callback === "function") callback();
    };
    ReadStream.prototype.on = function (type, listener) {
        events.EventEmitter.prototype.on.call(this, type, listener);
        // Auto-flow like a Node flowing stream: the first `data` listener
        // starts the pump (now, or right after the deferred open).
        if (type === "data") {
            if (this.fd !== null) this.read();
            else this._flowOnData = true;
        }
        return this;
    };
    ReadStream.prototype.once = function (type, listener) {
        events.EventEmitter.prototype.once.call(this, type, listener);
        if (type === "data") {
            if (this.fd !== null) this.read();
            else this._flowOnData = true;
        }
        return this;
    };
    function WriteStream(path, options) {
        if (!(this instanceof WriteStream)) return new WriteStream(path, options);
        events.EventEmitter.call(this);
        this.path = toPath(path);
        this.fd = null;
        this._chunks = [];
        this._ended = false;
    }
    WriteStream.prototype = Object.create(stream.Stream.prototype);
    WriteStream.prototype.constructor = WriteStream;
    WriteStream.prototype.write = function (chunk, encoding, callback) {
        if (typeof encoding === "function") {
            callback = encoding;
            encoding = undefined;
        }
        const done = typeof callback === "function" ? callback : () => {};
        if (this._ended) {
            const late = new Error("write after end");
            this.emit("error", late);
            done(late);
            return false;
        }
        try {
            this._chunks.push(toBytes(chunk, encoding));
        } catch (error) {
            this.emit("error", error);
            done(error);
            return false;
        }
        done();
        return true;
    };
    WriteStream.prototype.end = function (chunk, encoding, callback) {
        if (typeof chunk === "function") {
            callback = chunk;
            chunk = undefined;
            encoding = undefined;
        } else if (typeof encoding === "function") {
            callback = encoding;
            encoding = undefined;
        }
        if (chunk !== undefined) this.write(chunk, encoding);
        this._ended = true;
        const done = typeof callback === "function" ? callback : () => {};
        try {
            for (let i = 0; i < this._chunks.length; i++) {
                __strake_fs_write(this.path, this._chunks[i], i > 0);
            }
        } catch (error) {
            this.emit("error", error);
            done(error);
            return;
        }
        this._chunks = [];
        // A wrapping opener (graceful-fs) may have parked an fd on us via
        // its own `open`; our commit path never uses it, so close it here
        // instead of leaking it.
        if (this.fd !== null) {
            try {
                __strake_fs_close(this.fd);
            } catch (e) {}
            this.fd = null;
        }
        this.emit("finish");
        done();
        this.emit("close");
    };
    WriteStream.prototype.close = function (callback) {
        this._ended = true;
        if (this.fd !== null) {
            try {
                __strake_fs_close(this.fd);
            } catch (e) {}
            this.fd = null;
        }
        this.emit("close");
        if (typeof callback === "function") callback();
    };
    const fs = {
        constants,
        Stats,
        ReadStream,
        WriteStream,
        FileReadStream: ReadStream,
        FileWriteStream: WriteStream,
        existsSync(path) {
            try {
                return __strake_fs_exists(toPath(path)) === true;
            } catch (e) {
                return false;
            }
        },
        accessSync(path, mode) {
            __strake_fs_access(toPath(path), mode === undefined ? 0 : Number(mode));
        },
        mkdirSync(path, options) {
            __strake_fs_mkdir(toPath(path), !!(options && options.recursive));
        },
        readFileSync(path, options) {
            const bytes = __strake_fs_read(toPath(path));
            const encoding = typeof options === "string" ? options : options && options.encoding;
            const out = Buffer.from(bytes);
            return encoding ? out.toString(encoding) : out;
        },
        writeFileSync(path, data, options) {
            const encoding = typeof options === "string" ? options : options && options.encoding;
            __strake_fs_write(toPath(path), toBytes(data, encoding), false);
        },
        appendFileSync(path, data, options) {
            const encoding = typeof options === "string" ? options : options && options.encoding;
            __strake_fs_write(toPath(path), toBytes(data, encoding), true);
        },
        statSync(path) {
            return new Stats(__strake_fs_stat(toPath(path), true));
        },
        lstatSync(path) {
            return new Stats(__strake_fs_stat(toPath(path), false));
        },
        readdirSync(path) {
            return __strake_fs_readdir(toPath(path));
        },
        readdir(path, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.readdirSync(path));
                } catch (error) {
                    callback(error);
                }
            });
        },
        unlinkSync(path) {
            __strake_fs_unlink(toPath(path));
        },
        renameSync(from, to) {
            __strake_fs_rename(toPath(from), toPath(to));
        },
        copyFileSync(from, to) {
            __strake_fs_copy(toPath(from), toPath(to));
        },
        realpathSync(path) {
            return __strake_fs_realpath(toPath(path));
        },
        readlinkSync(path, options) {
            const encoding = typeof options === "string" ? options : options && options.encoding;
            const target = __strake_fs_readlink(toPath(path));
            return encoding === "buffer" ? Buffer.from(target) : target;
        },
        utimesSync(path, atime, mtime) {
            __strake_fs_utimes(toPath(path), toUnixTime(atime), toUnixTime(mtime));
        },
        openSync(path, flags, mode) {
            return __strake_fs_open(toPath(path), flags === undefined ? "r" : flags, mode === undefined ? 438 : Number(mode));
        },
        closeSync(fd) {
            __strake_fs_close(Number(fd));
        },
        readSync(fd, buffer, offset, length, position) {
            return __strake_fs_read_fd(Number(fd), buffer, offset, length, position === undefined ? null : position);
        },
        writeSync(fd, data, offset, length, position) {
            if (typeof data === "string") {
                const pos = offset === undefined ? null : offset;
                const enc = length === undefined ? "utf8" : length;
                const bytes = Buffer.from(data, enc);
                return __strake_fs_write_fd(Number(fd), bytes, 0, bytes.length, pos);
            }
            const bytes = toBytes(data);
            const off = offset === undefined ? 0 : offset;
            const len = length === undefined ? bytes.length - off : length;
            return __strake_fs_write_fd(Number(fd), bytes, off, len, position === undefined ? null : position);
        },
        // Async variants defer through the microtask queue and run the same
        // natives underneath (true Tokio-backed async is issue #141). The
        // deferral is load-bearing, not cosmetic: real-world wrappers
        // (`graceful-fs` streams) attach listeners after construction and
        // assume `open` completes later — a synchronously-firing callback
        // would pump `data`/`end` into zero listeners. Argument validation
        // still throws synchronously, as in Node.
        open(path, flags, mode, callback) {
            if (typeof flags === "function") {
                callback = flags;
                flags = undefined;
                mode = undefined;
            } else if (typeof mode === "function") {
                callback = mode;
                mode = undefined;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.openSync(path, flags, mode));
                } catch (error) {
                    callback(error);
                }
            });
        },
        close(fd, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.closeSync(fd);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        read(fd, buffer, offset, length, position, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.readSync(fd, buffer, offset, length, position));
                } catch (error) {
                    callback(error);
                }
            });
        },
        write(fd, data, offset, length, position, callback) {
            if (typeof data === "string") {
                // (fd, string[, position[, encoding]], callback) overload.
                const rest = [offset, length, position, callback];
                const found = rest.filter((a) => typeof a === "function").pop();
                const nonFn = rest.filter((a) => typeof a !== "function" && a !== undefined);
                callback = found;
                if (typeof callback !== "function") throw new TypeError("callback must be a function");
                queueMicrotask(() => {
                    try {
                        callback(null, fs.writeSync(fd, data, nonFn.length > 0 ? nonFn[0] : null, nonFn.length > 1 ? nonFn[1] : "utf8"));
                    } catch (error) {
                        callback(error);
                    }
                });
                return;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.writeSync(fd, data, offset, length, position));
                } catch (error) {
                    callback(error);
                }
            });
        },
        readFile(path, options, callback) {
            if (typeof options === "function") {
                callback = options;
                options = undefined;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.readFileSync(path, options));
                } catch (error) {
                    callback(error);
                }
            });
        },
        writeFile(path, data, options, callback) {
            if (typeof options === "function") {
                callback = options;
                options = undefined;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.writeFileSync(path, data, options);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        appendFile(path, data, options, callback) {
            if (typeof options === "function") {
                callback = options;
                options = undefined;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.appendFileSync(path, data, options);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        readdir(path, options, callback) {
            if (typeof options === "function") {
                callback = options;
                options = undefined;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.readdirSync(path, options));
                } catch (error) {
                    callback(error);
                }
            });
        },
        stat(path, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.statSync(path));
                } catch (error) {
                    callback(error);
                }
            });
        },
        rename(from, to, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.renameSync(from, to);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        unlink(path, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.unlinkSync(path);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        mkdir(path, options, callback) {
            if (typeof options === "function") {
                callback = options;
                options = undefined;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.mkdirSync(path, options);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        copyFile(from, to, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.copyFileSync(from, to);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        access(path, mode, callback) {
            if (typeof mode === "function") {
                callback = mode;
                mode = undefined;
            }
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    fs.accessSync(path, mode);
                    callback(null);
                } catch (error) {
                    callback(error);
                }
            });
        },
        realpath(path, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                try {
                    callback(null, fs.realpathSync(path));
                } catch (error) {
                    callback(error);
                }
            });
        },
        exists(path, callback) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            queueMicrotask(() => {
                let found = false;
                try {
                    found = fs.existsSync(path);
                } catch (e) {
                    found = false;
                }
                callback(found);
            });
        },
        createReadStream(path, options) {
            return new ReadStream(path, options);
        },
        createWriteStream(path, options) {
            return new WriteStream(path, options);
        },
    };
    // `fs/promises` (issue #155): Joplin awaits these all over startup
    // (`graceful-fs` touches `lstat`/`readdir`/`readlink`/`realpath` at
    // import). Same sync I/O underneath, completed on microtasks — never
    // sync, so `await` ordering matches Node.
    const promises = {
        readFile: (path, options) => Promise.resolve().then(() => fs.readFileSync(path, options)),
        writeFile: (path, data, options) => Promise.resolve().then(() => fs.writeFileSync(path, data, options)),
        appendFile: (path, data, options) => Promise.resolve().then(() => fs.appendFileSync(path, data, options)),
        stat: (path) => Promise.resolve().then(() => fs.statSync(path)),
        lstat: (path) => Promise.resolve().then(() => fs.lstatSync(path)),
        readdir: (path) => Promise.resolve().then(() => fs.readdirSync(path)),
        mkdir: (path, options) => Promise.resolve().then(() => fs.mkdirSync(path, options)),
        unlink: (path) => Promise.resolve().then(() => fs.unlinkSync(path)),
        rename: (from, to) => Promise.resolve().then(() => fs.renameSync(from, to)),
        copyFile: (from, to) => Promise.resolve().then(() => fs.copyFileSync(from, to)),
        access: (path, mode) => Promise.resolve().then(() => fs.accessSync(path, mode)),
        realpath: (path) => Promise.resolve().then(() => fs.realpathSync(path)),
        readlink: (path, options) => Promise.resolve().then(() => fs.readlinkSync(path, options)),
        utimes: (path, atime, mtime) => Promise.resolve().then(() => fs.utimesSync(path, atime, mtime)),
    };
    fs.promises = promises;
    table["node:fs"] = fs;
    table["fs"] = fs;
    table["node:fs/promises"] = promises;
    table["fs/promises"] = promises;
})();
"#;

/// Mutable Electron main-process state shared between the native primitives
/// (which run inside JS calls) and the [`ElectronHost`] handle held by the
/// embedder. Single-threaded by construction (Boa contexts are `!Send`).
struct ElectronHostState {
    app: App,
    /// Privileged custom schemes (`protocol.registerSchemesAsPrivileged`,
    /// issue #155): declared at load by main-process bundles.
    protocol: ProtocolRegistry,
    /// Electron `session` registry (`session.fromPath`/`fromPartition`,
    /// issue #155): profile and partition sessions plus the default.
    sessions: SessionRegistry,
    /// `session.protocol.handle` JS handlers by (session, scheme), kept for
    /// future dispatch; the scheme names live in the compat core.
    session_protocol_handlers: HashMap<(SessionId, String), JsObject>,
    /// `session.webRequest` listener registrations by session (issue #155):
    /// recorded with their URL filters, not yet enforced (no renderer
    /// network stack consumes them).
    session_web_request: HashMap<SessionId, Vec<RecordedWebRequestRule>>,
    windows: WindowManager,
    /// Display snapshot backing `screen.*` (issue #96): the headless fallback
    /// until the embedder installs real winit monitor metrics.
    screen: Screen,
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
    /// `win.on('closed')` JS listeners by window id (issue #84).
    window_closed_listeners: HashMap<u32, Vec<JsObject>>,
    /// `webContents.on(event, cb)` JS listeners by (window, event)
    /// (issue #155): recorded for future dispatch — headless has no
    /// renderer to fire load/crash/unresponsive events yet.
    web_contents_listeners: HashMap<(u32, String), Vec<JsObject>>,
    /// `webContents.setWindowOpenHandler` JS handlers by window (issue
    /// #155): kept for the `window.open` interception follow-up.
    window_open_handlers: HashMap<u32, JsObject>,
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
    /// Color-scheme hub backing `nativeTheme` (issue #155): headless
    /// defaults to light until the shell installs the OS scheme via
    /// `note_system_change`.
    native_theme: NativeTheme,
    /// CommonJS module cache (canonical path -> exports) for the
    /// relative-file / `node_modules` loader (issue #151). Cached before
    /// evaluation so circular requires observe partial exports, matching
    /// Node's semantics.
    module_cache: HashMap<String, JsValue>,
    /// Stack of currently-loading module files for relative resolution and
    /// circular-require detection (issue #151). Empty during the top-level
    /// main-script eval, where `./x` resolves against the app root.
    require_stack: Vec<PathBuf>,
    /// Capability grants for the `node:fs` sync primitives (issue #154):
    /// deny-by-default, so a host without an explicit manifest refuses every
    /// filesystem operation (`EACCES`, never an existence oracle).
    permissions: Enforcer,
    /// Open fd table for `node:fs` (issue #155): virtual fds map to host
    /// files here. Fresh per host — snapshots never carry open files
    /// across contexts, and rights were fixed at `open`, POSIX-style.
    fs_fds: HashMap<u32, crate::node_fs::OpenFd>,
    fs_next_fd: u32,
    /// Connected TCP handles for `net.Socket` (issue #155): connect truth
    /// without the duplex bridge — reads/writes land with it. Fresh per
    /// host, like the fd table above.
    net_sockets: HashMap<u64, std::net::TcpStream>,
    net_next_socket: u64,
}

/// One recorded `session.webRequest` listener: the interception point name
/// plus the URL patterns it applies to (empty when the bundle passes no
/// filter, as Sentry's `onHeadersReceived` does).
struct RecordedWebRequestRule {
    kind: &'static str,
    urls: Vec<String>,
    // Kept for the dispatch follow-up (no renderer network stack consumes
    // webRequest listeners yet); read then.
    #[allow(dead_code)]
    listener: JsObject,
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

impl SharedElectronHost {
    /// Run `f` against the capability enforcer (issue #154). Statement-scoped
    /// takes only: the borrow ends when `f` returns, so re-entrant JS cannot
    /// trip the `RefCell`.
    pub(crate) fn with_permissions<R>(&self, f: impl FnOnce(&mut Enforcer) -> R) -> R {
        f(&mut self.0.borrow_mut().permissions)
    }

    /// Allocate a virtual fd for an opened host file (issue #155).
    pub(crate) fn fs_fd_open(&self, handle: crate::node_fs::OpenFd) -> u32 {
        let mut state = self.0.borrow_mut();
        let fd = state.fs_next_fd;
        state.fs_next_fd = fd.wrapping_add(1);
        state.fs_fds.insert(fd, handle);
        fd
    }

    /// Drop an fd; `false` reads `EBADF` at the call site.
    pub(crate) fn fs_fd_close(&self, fd: u32) -> bool {
        self.0.borrow_mut().fs_fds.remove(&fd).is_some()
    }

    /// Run `f` against one open fd; `None` reads `EBADF` at the call site.
    pub(crate) fn fs_fd_with<R>(
        &self,
        fd: u32,
        f: impl FnOnce(&mut crate::node_fs::OpenFd) -> R,
    ) -> Option<R> {
        Some(f(self.0.borrow_mut().fs_fds.get_mut(&fd)?))
    }
}

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
                protocol: ProtocolRegistry::new(),
                sessions: SessionRegistry::new(),
                session_protocol_handlers: HashMap::new(),
                session_web_request: HashMap::new(),
                windows: WindowManager::new(),
                screen: Screen::default(),
                module: None,
                app_listeners: HashMap::new(),
                when_ready_resolvers: Vec::new(),
                ipc_handlers: HashMap::new(),
                ipc_listeners: HashMap::new(),
                created_window_ids: Vec::new(),
                window_closed_listeners: HashMap::new(),
                web_contents_listeners: HashMap::new(),
                window_open_handlers: HashMap::new(),
                renderer_module: None,
                renderer_listeners: HashMap::new(),
                invoke_queue: VecDeque::new(),
                send_queue: VecDeque::new(),
                clipboard: Clipboard::default(),
                notifications: NotificationCenter::recording(),
                notification_clicks: HashMap::new(),
                power: PowerHub::new(),
                native_theme: NativeTheme::new(false),
                power_listeners: HashMap::new(),
                safe_storage: SafeStorage::recording(),
                module_cache: HashMap::new(),
                require_stack: Vec::new(),
                permissions: Enforcer::new(PermissionManifest::default()),
                fs_fds: HashMap::new(),
                fs_next_fd: crate::node_fs::FIRST_FD,
                net_sockets: HashMap::new(),
                net_next_socket: 1,
            }))),
        }
    }

    /// Grant filesystem (and future) capabilities to app code (issue #154):
    /// the embedder-approved manifest behind `require('fs')`. Without this
    /// call the host stays deny-by-default.
    pub fn with_permissions(self, manifest: PermissionManifest) -> Self {
        self.shared.0.borrow_mut().permissions = Enforcer::new(manifest);
        self
    }

    /// Snapshot the boot main-process state for one headed paint (issue
    /// #147): a fresh host carrying the app identity (plus readiness), the
    /// display snapshot, the capability grants, and a re-created window
    /// registry — but none of the boot context's JS-bound registrations
    /// (module objects, `ipcMain`/`app`/window listeners, queued calls, the
    /// module cache), which cannot cross Boa contexts. Paint snapshots
    /// instead of sharing because renderer installs overwrite the shared
    /// `renderer_module` slot: installing paint into the live boot host
    /// would clobber the boot renderer.
    ///
    /// Window ids reassign from zero in ascending creation order (count and
    /// order preserved; ids match whenever no window was closed). Per-window
    /// options, bounds, resizability, titles, visibility, opener links, and
    /// pending navigation targets carry over; transient chrome state
    /// (minimized/maximized/focused) and queued main-to-renderer sends do
    /// not — headed paint is a fresh render, not a session restore.
    /// `ipcMain` handlers stay behind with the boot context (JS functions
    /// cannot cross contexts); `ipcRenderer.invoke` from a paint preload
    /// queues with no pump, exactly like a boot preload before its pump.
    pub fn snapshot_for_paint(&self) -> Self {
        let state = self.shared.0.borrow();
        let mut app = App::new(state.app.name(), state.app.version());
        if state.app.is_ready() {
            app.mark_ready();
        }
        let mut windows = WindowManager::new();
        let mut id_map: HashMap<u32, u32> = HashMap::new();
        for old_id in state.windows.live_ids() {
            let Some(win) = state.windows.get(old_id) else {
                continue;
            };
            // Parents predate children in ascending id order, so a mapped
            // opener is always live already; a dead opener falls back to a
            // top-level window rather than dropping the child.
            let new_id = win
                .opener()
                .and_then(|old_opener| id_map.get(&old_opener))
                .and_then(|new_opener| windows.open_child(*new_opener, win.options().clone()))
                .unwrap_or_else(|| windows.create(win.options().clone()));
            id_map.insert(old_id, new_id);
            windows.set_bounds(new_id, win.bounds());
            windows.set_resizable(new_id, win.is_resizable());
            windows.set_title(new_id, win.title());
            if win.is_visible() {
                windows.show(new_id);
            } else {
                windows.hide(new_id);
            }
            if let Some(target) = win.web_contents().pending_url() {
                // Both setters normalize idempotently, so a recorded target
                // restores byte-identically.
                if let Some(restored) = windows.get_mut(new_id) {
                    if target.starts_with("file://") {
                        restored.web_contents_mut().load_file(target);
                    } else {
                        restored.web_contents_mut().load_url(target);
                    }
                }
            }
            let doc_title = win.web_contents().get_title().to_string();
            if let Some(restored) = windows.get_mut(new_id) {
                restored
                    .web_contents_mut()
                    .set_document_title(Some(doc_title));
            }
        }
        let screen = state.screen.clone();
        let permissions = state.permissions.clone();
        let sessions = state.sessions.clone();
        let native_theme = state.native_theme.clone();
        drop(state);
        let snapshot = Self::new("", "");
        {
            let mut fresh = snapshot.shared.0.borrow_mut();
            fresh.app = app;
            fresh.windows = windows;
            fresh.screen = screen;
            fresh.permissions = permissions;
            fresh.sessions = sessions;
            fresh.native_theme = native_theme;
        }
        snapshot
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

    /// Content bounds of a window (`getBounds`), if it is live.
    pub fn window_bounds(&self, id: u32) -> Option<strake_electron_compat::Bounds> {
        self.shared.0.borrow().windows.get_bounds(id)
    }

    /// Visibility of a window (`show`/`hide`); destroyed ids read `false`.
    pub fn window_visible(&self, id: u32) -> bool {
        self.shared.0.borrow().windows.is_visible(id)
    }

    /// `resizable` flag of a window, if it is live.
    pub fn window_resizable(&self, id: u32) -> Option<bool> {
        self.shared
            .0
            .borrow()
            .windows
            .get(id)
            .map(|win| win.is_resizable())
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

    /// Ids of live windows in ascending creation order
    /// (`BrowserWindow.getAllWindows()`, issue #107).
    pub fn live_window_ids(&self) -> Vec<u32> {
        self.shared.0.borrow().windows.live_ids()
    }

    /// Recorded `webPreferences.preload` path of a window, if it declared
    /// one and is still live (issue #109).
    pub fn window_preload(&self, id: u32) -> Option<String> {
        self.shared
            .0
            .borrow()
            .windows
            .get(id)
            .and_then(|win| win.options().web_preferences.preload.clone())
    }

    /// `(window id, preload path)` for live windows declaring a preload, in
    /// ascending window-id order: the embedder's execution queue (issue #109).
    pub fn pending_preloads(&self) -> Vec<(u32, String)> {
        self.shared.0.borrow().windows.pending_preloads()
    }

    /// Session ids in creation order (id `0` is the default session).
    pub fn session_ids(&self) -> Vec<SessionId> {
        self.shared.0.borrow().sessions.ids()
    }

    /// Schemes registered via `session.protocol.handle` for a session.
    pub fn session_handled_schemes(&self, id: SessionId) -> Vec<String> {
        self.shared
            .0
            .borrow()
            .sessions
            .get(id)
            .map(|session| session.handled_schemes().to_vec())
            .unwrap_or_default()
    }

    /// `(interception point, URL patterns)` webRequest rules recorded for
    /// a session, in registration order.
    pub fn session_web_request_rules(&self, id: SessionId) -> Vec<(String, Vec<String>)> {
        self.shared
            .0
            .borrow()
            .session_web_request
            .get(&id)
            .map(|rules| {
                rules
                    .iter()
                    .map(|rule| (rule.kind.to_string(), rule.urls.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether a `webContents.setWindowOpenHandler` handler is recorded
    /// for a window.
    pub fn web_contents_has_window_open_handler(&self, id: u32) -> bool {
        self.shared
            .0
            .borrow()
            .window_open_handlers
            .contains_key(&id)
    }

    /// `webContents.on` event names subscribed for a window, sorted.
    pub fn web_contents_listener_events(&self, id: u32) -> Vec<String> {
        let mut events: Vec<String> = self
            .shared
            .0
            .borrow()
            .web_contents_listeners
            .keys()
            .filter(|(window, _)| *window == id)
            .map(|(_, event)| event.clone())
            .collect();
        events.sort();
        events
    }

    /// Owning session of a window (`webPreferences.session`), if live.
    pub fn window_session(&self, id: u32) -> Option<SessionId> {
        self.shared
            .0
            .borrow()
            .windows
            .get(id)
            .map(|win| win.options().session)
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

    /// Queued main-to-renderer `webContents.send` payloads across all windows
    /// (issue #91), drained by the pump into renderer `ipcRenderer.on`
    /// listeners.
    pub fn pending_main_send_count(&self) -> usize {
        self.shared.0.borrow().windows.queued_main_send_count()
    }

    /// Install real display metrics (issue #96), replacing the headless
    /// fallback snapshot that backs `screen.*`.
    ///
    /// Deferred producer note: nothing outside tests calls this yet — no code
    /// converts winit monitor handles (position/size/scale factor) into a
    /// [`Screen`], so the live shim keeps serving the `Screen::default`
    /// 1024x768 fallback until that winit bridge lands (issue #96,
    /// criterion 1) in a later slice.
    pub fn set_screen(&self, screen: Screen) {
        self.shared.0.borrow_mut().screen = screen;
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
pub(crate) fn electron_state(context: &mut Context) -> JsResult<SharedElectronHost> {
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

pub(crate) fn require_string_arg(args: &[JsValue], index: usize, what: &str) -> JsResult<String> {
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

/// Look up a Node core stand-in (`node:path`, `node:url`, `node:process`,
/// plus the unprefixed aliases) from the per-context
/// `globalThis.__strake_node_modules` table (issue #108). Anything else is
/// `None`, and the caller throws Node's "Cannot find module" error (`fs`
/// resolves through its own shell per issue #154; anything else unserved
/// stays a named miss).
fn node_standin_module(context: &mut Context, specifier: &str) -> JsResult<Option<JsValue>> {
    let table = context
        .global_object()
        .get(js_string!("__strake_node_modules"), context)?;
    let Some(table) = table.as_object() else {
        return Ok(None);
    };
    let module = table.get(js_string!(specifier), context)?;
    if module.is_undefined() || module.is_null() {
        return Ok(None);
    }
    Ok(Some(module))
}

fn cannot_find_module(specifier: &str) -> JsError {
    JsError::from(JsNativeError::error().with_message(format!("Cannot find module '{specifier}'")))
}

/// Node core modules that must never resolve via the file loader (issue
/// #151): `node:`-prefixed cores beyond the module table plus unprefixed
/// core names. Resolving them from `node_modules` would silently mis-resolve
/// a core as third-party.
fn is_node_core(specifier: &str) -> bool {
    if specifier.starts_with("node:") {
        return true;
    }
    matches!(
        specifier,
        "fs" | "path"
            | "url"
            | "events"
            | "process"
            | "child_process"
            | "os"
            | "util"
            | "assert"
            | "buffer"
            | "crypto"
            | "http"
            | "https"
            | "stream"
            | "querystring"
            | "net"
            | "tls"
            | "dns"
            | "dgram"
            | "cluster"
            | "worker_threads"
            | "perf_hooks"
            | "async_hooks"
            | "readline"
            | "repl"
            | "tty"
            | "v8"
            | "vm"
            | "zlib"
            | "string_decoder"
            | "fs/promises"
            | "domain"
            | "timers"
            | "timers/promises"
            | "http2"
    )
}

/// App root for module resolution (issue #151): `__dirname` when the runner
/// set it via `set_node_app_root`, else `__strake_node_info.appRoot`, else
/// the process working directory.
pub(crate) fn app_root_dir(context: &mut Context) -> PathBuf {
    if let Ok(dirname) = context
        .global_object()
        .get(js_string!("__dirname"), context)
        && let Some(s) = dirname.as_string()
    {
        let path = PathBuf::from(s.to_std_string_escaped());
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    if let Ok(info) = context
        .global_object()
        .get(js_string!("__strake_node_info"), context)
        && let Some(obj) = info.as_object()
        && let Ok(root) = obj.get(js_string!("appRoot"), context)
        && let Some(s) = root.as_string()
    {
        return PathBuf::from(s.to_std_string_escaped());
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Probe Node's file resolution for one joined base (issue #151): exact file,
/// then `.js` / `.json` appended, then `index.js` / `index.json` inside
/// directories. `.node` native addons are never resolved (full C ABI is
/// issue #144); refusing avoids silent mis-resolution.
fn probe_file(base: &std::path::Path) -> Option<PathBuf> {
    if base.is_file() {
        if base.extension().is_some_and(|ext| ext == "node") {
            return None;
        }
        return Some(base.to_path_buf());
    }
    let base_str = base.to_string_lossy();
    if base_str.ends_with(".node") {
        return None;
    }
    for ext in [".js", ".json"] {
        let candidate = PathBuf::from(format!("{base_str}{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if base.is_dir() {
        for index in ["index.js", "index.json"] {
            let candidate = base.join(index);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve `./`, `../`, `/` specifiers against the requiring file's directory
/// (issue #151). The top-level main script has no stack entry, so `./x`
/// resolves against the app root.
fn resolve_relative(specifier: &str, parent_dir: &std::path::Path) -> Option<PathBuf> {
    let joined = if specifier.starts_with('/') {
        PathBuf::from(specifier)
    } else {
        parent_dir.join(specifier)
    };
    probe_file(&joined)
}

/// Split a bare specifier into its package name and optional subpath,
/// handling `@scope/name` (issue #151).
fn split_bare(specifier: &str) -> (String, Option<String>) {
    if let Some(rest) = specifier.strip_prefix('@') {
        let mut parts = rest.splitn(3, '/');
        let scope = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if name.is_empty() {
            return (String::new(), None);
        }
        let pkg = format!("@{scope}/{name}");
        let sub = parts.next().map(str::to_string);
        (pkg, sub)
    } else {
        let mut parts = specifier.splitn(2, '/');
        let pkg = parts.next().unwrap_or_default().to_string();
        let sub = parts.next().map(str::to_string);
        (pkg, sub)
    }
}

/// Resolve a package directory's entry point via `package.json` `main`,
/// falling back to `index.js` / `index.json` (issue #151).
fn resolve_package_main(pkg_dir: &std::path::Path) -> Option<PathBuf> {
    let pkg_json = pkg_dir.join("package.json");
    if let Ok(content) = std::fs::read_to_string(&pkg_json)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(main) = value.get("main").and_then(|main| main.as_str())
        && !main.is_empty()
    {
        let base = pkg_dir.join(main);
        if let Some(probed) = probe_file(&base) {
            return Some(probed);
        }
    }
    for index in ["index.js", "index.json"] {
        let candidate = pkg_dir.join(index);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Resolve bare third-party specifiers from the app's `node_modules`,
/// walking up from the requiring file toward the filesystem root (issue
/// #151). Returns `None` when no candidate exists so the caller throws
/// Node's "Cannot find module" error naming the package.
fn resolve_bare(
    specifier: &str,
    start_dir: &std::path::Path,
    _app_root: &std::path::Path,
) -> Option<PathBuf> {
    let (pkg, sub) = split_bare(specifier);
    if pkg.is_empty() {
        return None;
    }
    let mut current = Some(start_dir.to_path_buf());
    while let Some(dir) = current {
        let mut base = dir.join("node_modules").join(&pkg);
        if let Some(subpath) = &sub {
            base = base.join(subpath);
            if let Some(probed) = probe_file(&base) {
                return Some(probed);
            }
        } else if base.is_dir() {
            if let Some(entry) = resolve_package_main(&base) {
                return Some(entry);
            }
        } else if let Some(probed) = probe_file(&base) {
            return Some(probed);
        }
        current = dir.parent().map(std::path::Path::to_path_buf);
    }
    None
}

/// Resolve any loadable specifier to a file path (issue #151): relative and
/// absolute paths via [`resolve_relative`], bare packages via
/// [`resolve_bare`]. Returns `None` for Electron/stand-in/core specifiers
/// (handled before the loader) and for unresolvable paths.
fn resolve_commonjs(
    specifier: &str,
    shared: &SharedElectronHost,
    app_root: &std::path::Path,
) -> Option<PathBuf> {
    if specifier.is_empty()
        || specifier == "electron"
        || is_node_core(specifier)
        || specifier.starts_with("http:")
        || specifier.starts_with("https:")
        || specifier.starts_with("file:")
        || specifier.starts_with("data:")
    {
        return None;
    }
    if specifier.starts_with("./") || specifier.starts_with("../") || specifier.starts_with('/') {
        let parent_dir = shared
            .0
            .borrow()
            .require_stack
            .last()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_else(|| app_root.to_path_buf());
        return resolve_relative(specifier, &parent_dir);
    }
    // Bare specifiers must not look like relative paths without a prefix;
    // anything else goes to `node_modules`.
    if specifier.starts_with('.') {
        return None;
    }
    let start_dir = shared
        .0
        .borrow()
        .require_stack
        .last()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| app_root.to_path_buf());
    resolve_bare(specifier, &start_dir, app_root)
}

/// Load one resolved CommonJS file (issue #151): JSON files parse to objects,
/// JS files evaluate with temporary `module` / `exports` / `__dirname` /
/// `__filename` globals. The exports object is cached before evaluation so
/// circular requires observe partial exports, matching Node. Failed
/// evaluations are removed from the cache and their JS error propagates.
fn load_resolved_file(
    path: &std::path::Path,
    shared: &SharedElectronHost,
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = std::fs::canonicalize(path)
        .map(|canonical| canonical.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    if let Some(cached) = shared.0.borrow().module_cache.get(&key).cloned() {
        return Ok(cached);
    }
    if path.extension().is_some_and(|ext| ext == "json") {
        let content = std::fs::read_to_string(path)
            .map_err(|error| cannot_find_module(&format!("{} ({error})", path.display())))?;
        let value: serde_json::Value = serde_json::from_str(&content)
            .map_err(|error| cannot_find_module(&format!("{} ({error})", path.display())))?;
        let js = json_to_js(&value, context)?;
        shared.0.borrow_mut().module_cache.insert(key, js.clone());
        return Ok(js);
    }
    let source = std::fs::read_to_string(path)
        .map_err(|error| cannot_find_module(&format!("{} ({error})", path.display())))?;
    // Fresh `module = { exports: {} }` pair, cached before evaluation for
    // circular requires.
    let exports_obj = ObjectInitializer::new(context).build();
    let mut module_init = ObjectInitializer::new(context);
    module_init.property(
        js_string!("exports"),
        JsValue::from(exports_obj.clone()),
        Attribute::all(),
    );
    let module_obj = module_init.build();
    shared
        .0
        .borrow_mut()
        .module_cache
        .insert(key.clone(), JsValue::from(exports_obj.clone()));
    shared.0.borrow_mut().require_stack.push(path.to_path_buf());
    // Save the globals this module shadows, then point them at this file.
    let global = context.global_object();
    let saved_dirname = global
        .get(js_string!("__dirname"), context)
        .unwrap_or_default();
    let saved_filename = global
        .get(js_string!("__filename"), context)
        .unwrap_or_default();
    let saved_module = global
        .get(js_string!("module"), context)
        .unwrap_or_default();
    let saved_exports = global
        .get(js_string!("exports"), context)
        .unwrap_or_default();
    let parent_dir = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    let dirname_js = JsValue::from(js_string!(parent_dir.to_string_lossy().as_ref()));
    let filename_js = JsValue::from(js_string!(path.to_string_lossy().as_ref()));
    let eval_result = (|| -> JsResult<JsValue> {
        global.set(js_string!("__dirname"), dirname_js.clone(), false, context)?;
        global.set(
            js_string!("__filename"),
            filename_js.clone(),
            false,
            context,
        )?;
        global.set(
            js_string!("module"),
            JsValue::from(module_obj.clone()),
            false,
            context,
        )?;
        global.set(
            js_string!("exports"),
            JsValue::from(exports_obj.clone()),
            false,
            context,
        )?;
        // CommonJS function wrapper (Node parity): each file evaluates in its
        // own function scope, so top-level `const`/`let`/`class` never
        // collide across files sharing the global scope (Joplin boot hit
        // `duplicate lexical declaration` via `@electron/remote`'s siblings).
        // `this` is `module.exports`, as in Node.
        let wrapped = format!(
            "(function (exports, require, module, __filename, __dirname) {{\n{source}\n}})"
        );
        // Same keyword-arrow fallback as eval'd sources (issue #155):
        // real bundlers emit bare `of =>` params, and split bundles load
        // here, not as the main entry. The repair runs only after a proven
        // parse failure, so module code never evaluates twice.
        let wrapper = match context.eval(Source::from_bytes(&wrapped)) {
            Ok(value) => value,
            Err(error) => {
                let parse_failure =
                    Script::parse(Source::from_bytes(&wrapped), None, context).is_err();
                if !parse_failure {
                    return Err(error);
                }
                match crate::keyword_arrow::repair_keyword_arrow_params(&wrapped, context) {
                    Some(fixed) => context.eval(Source::from_bytes(&fixed))?,
                    None => return Err(error),
                }
            }
        };
        let wrapper = wrapper
            .as_object()
            .filter(|obj| obj.is_callable())
            .ok_or_else(|| {
                JsError::from(JsNativeError::typ().with_message("module wrapper is not callable"))
            })?;
        let require_fn = global.get(js_string!("require"), context)?;
        let call_args = [
            JsValue::from(exports_obj.clone()),
            require_fn,
            JsValue::from(module_obj.clone()),
            filename_js,
            dirname_js,
        ];
        wrapper.call(&JsValue::from(exports_obj.clone()), &call_args, context)?;
        module_obj.get(js_string!("exports"), context)
    })();
    // Always restore the shadowed globals and pop the stack, even on throw.
    let _ = global.set(js_string!("__dirname"), saved_dirname, false, context);
    let _ = global.set(js_string!("__filename"), saved_filename, false, context);
    let _ = global.set(js_string!("module"), saved_module, false, context);
    let _ = global.set(js_string!("exports"), saved_exports, false, context);
    shared.0.borrow_mut().require_stack.pop();
    match eval_result {
        Ok(final_exports) => {
            shared
                .0
                .borrow_mut()
                .module_cache
                .insert(key, final_exports.clone());
            Ok(final_exports)
        }
        Err(error) => {
            shared.0.borrow_mut().module_cache.remove(&key);
            Err(error)
        }
    }
}

/// `require(specifier)`: `'electron'` plus the Node core stand-ins
/// (`node:path`, `node:url`, `node:process`, `node:events`), plus the
/// relative-file and `node_modules` CommonJS loader (issue #151); anything
/// else throws Node's "Cannot find module" error.
fn e_require(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let specifier = require_string_arg(args, 0, "require")?;
    if specifier == "electron" {
        let shared = electron_state(context)?;
        return shared
            .0
            .borrow()
            .module
            .clone()
            .map(JsValue::from)
            .ok_or_else(|| {
                JsNativeError::error()
                    .with_message("Electron module not initialised")
                    .into()
            });
    }
    if let Some(module) = node_standin_module(context, &specifier)? {
        return Ok(module);
    }
    if !is_node_core(&specifier) {
        let shared = electron_state(context)?;
        let app_root = app_root_dir(context);
        if let Some(path) = resolve_commonjs(&specifier, &shared, &app_root) {
            return load_resolved_file(&path, &shared, context);
        }
    }
    Err(cannot_find_module(&specifier))
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

/// Where an `app.getPath`/`setPath` name resolves (issue #155).
enum ElectronPathTarget {
    /// Named slot in the compat core (`App::get_path` / `set_path`).
    Core(AppPath),
    /// The current executable (`std::env::current_exe`); read-only.
    Exe,
    /// The directory holding the app code; read-only.
    ModuleDir,
}

/// Map an `app.getPath`/`setPath` name to its target. `sessionData` aliases
/// `userData` (its Electron default — the core keeps no separate
/// session-data slot). `logs`/`music`/`pictures`/`videos`/`crashDumps` are
/// real Electron names but have no core slot and no OS derivation, so they
/// report as unavailable rather than inventing directories.
fn electron_path_target(name: &str) -> Result<ElectronPathTarget, String> {
    match name {
        "home" => Ok(ElectronPathTarget::Core(AppPath::Home)),
        "appData" => Ok(ElectronPathTarget::Core(AppPath::AppData)),
        "userData" | "sessionData" => Ok(ElectronPathTarget::Core(AppPath::UserData)),
        "temp" => Ok(ElectronPathTarget::Core(AppPath::Temp)),
        "desktop" => Ok(ElectronPathTarget::Core(AppPath::Desktop)),
        "documents" => Ok(ElectronPathTarget::Core(AppPath::Documents)),
        "downloads" => Ok(ElectronPathTarget::Core(AppPath::Downloads)),
        "exe" => Ok(ElectronPathTarget::Exe),
        "module" => Ok(ElectronPathTarget::ModuleDir),
        "logs" | "music" | "pictures" | "videos" | "crashDumps" => Err(format!(
            "Path '{name}' is not available in this embedder (no backing store)"
        )),
        _ => Err(format!("Unknown path '{name}'")),
    }
}

fn electron_path_error(message: String) -> JsError {
    JsError::from(JsNativeError::error().with_message(message))
}

/// `app.getAppPath()`: the resolved app root directory — the same root
/// module resolution uses, so app-relative paths resolve identically.
fn e_app_get_app_path(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let root = app_root_dir(context);
    Ok(JsValue::from(js_string!(root.to_string_lossy().as_ref())))
}

/// `app.getPath(name)`: core-backed names resolve once the embedder sets
/// them (`Temp` always resolves); unset names throw fail-closed — falling
/// back to `Temp` would scatter profile data into the OS temp dir.
fn e_app_get_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = require_string_arg(args, 0, "app.getPath")?;
    let path = match electron_path_target(&name).map_err(electron_path_error)? {
        ElectronPathTarget::Core(kind) => electron_state(context)?
            .0
            .borrow()
            .app
            .get_path(kind)
            .ok_or_else(|| {
                electron_path_error(format!(
                    "Path '{name}' is not configured (embedder must app.setPath it)"
                ))
            })?,
        ElectronPathTarget::Exe => std::env::current_exe()
            .map_err(|err| electron_path_error(format!("Path 'exe' is not available: {err}")))?,
        ElectronPathTarget::ModuleDir => app_root_dir(context),
    };
    Ok(JsValue::from(js_string!(path.to_string_lossy().as_ref())))
}

/// `app.setName(name)`: rename the app (issue #155); later `getName()`
/// calls report the new name.
fn e_app_set_name(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = require_string_arg(args, 0, "app.setName")?;
    electron_state(context)?.0.borrow_mut().app.set_name(&name);
    Ok(JsValue::undefined())
}

/// `app.setAppUserModelId(id)`: record the id (issue #155). Headless has
/// no taskbar integration, so like Electron off-Windows there is no
/// further effect and the call returns undefined.
fn e_app_set_app_user_model_id(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = require_string_arg(args, 0, "app.setAppUserModelId")?;
    electron_state(context)?
        .0
        .borrow_mut()
        .app
        .set_app_user_model_id(&id);
    Ok(JsValue::undefined())
}

/// `app.setAsDefaultProtocolClient(protocol)`: record the registration
/// (issue #155) and report success like Electron; the OS handler effect
/// stays deferred (see coverage). Extra `path`/`args` parameters are
/// accepted and ignored — they only refine the OS registration.
fn e_app_set_as_default_protocol_client(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let protocol = require_string_arg(args, 0, "app.setAsDefaultProtocolClient")?;
    electron_state(context)?
        .0
        .borrow_mut()
        .app
        .set_as_default_protocol_client(&protocol);
    Ok(JsValue::from(true))
}

fn protocol_type_error(message: String) -> JsError {
    JsError::from(JsNativeError::typ().with_message(message))
}

/// `protocol.registerSchemesAsPrivileged(customSchemes)`: validate and
/// record `{ scheme, privileges }` declarations (issue #155) for the
/// compat `ProtocolRegistry`. Shape errors are `TypeError`s, as in
/// Electron; unknown privilege names pass through rather than rejecting
/// future flags.
fn e_protocol_register_schemes_as_privileged(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    const WHAT: &str = "protocol.registerSchemesAsPrivileged";
    let raw = args.first().ok_or_else(|| {
        protocol_type_error(format!(
            "{WHAT} requires an array of {{ scheme, privileges }}"
        ))
    })?;
    let json = js_to_json(raw, context).map_err(|_| {
        protocol_type_error(format!(
            "{WHAT} requires an array of {{ scheme, privileges }}"
        ))
    })?;
    let list = json.as_array().ok_or_else(|| {
        protocol_type_error(format!(
            "{WHAT} requires an array of {{ scheme, privileges }}"
        ))
    })?;
    let mut schemes = Vec::with_capacity(list.len());
    for entry in list {
        let scheme = entry
            .get("scheme")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                protocol_type_error(format!(
                    "{WHAT} requires each scheme to have a string `scheme`"
                ))
            })?;
        let mut privileges = Vec::new();
        match entry.get("privileges") {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Object(flags)) => {
                for (name, enabled) in flags {
                    if enabled.as_bool().unwrap_or(false) {
                        privileges.push(name.clone());
                    }
                }
            }
            Some(_) => {
                return Err(protocol_type_error(format!(
                    "{WHAT} requires `privileges` to be an object when present"
                )));
            }
        }
        schemes.push(PrivilegedScheme {
            scheme: scheme.to_string(),
            privileges,
        });
    }
    electron_state(context)?
        .0
        .borrow_mut()
        .protocol
        .register_schemes(schemes);
    Ok(JsValue::undefined())
}

/// Numeric session id argument with a caller-named error.
fn session_id_arg(args: &[JsValue], context: &mut Context, what: &str) -> JsResult<SessionId> {
    let Some(first) = args.first() else {
        return Err(JsNativeError::typ()
            .with_message(format!("{what} requires a numeric session id"))
            .into());
    };
    let id = first.to_number(context)?;
    if id.is_finite() && id >= 0.0 {
        Ok(id as SessionId)
    } else {
        Err(JsNativeError::typ()
            .with_message(format!("{what} requires a numeric session id"))
            .into())
    }
}

/// `{ cache }` session option: missing/`undefined`/`null` default to `true`,
/// matching Electron's persistent-by-default sessions.
fn session_cache_arg(args: &[JsValue], index: usize) -> bool {
    match args.get(index) {
        None => true,
        Some(value) if value.is_undefined() || value.is_null() => true,
        Some(value) => value.to_boolean(),
    }
}

/// `session.fromPath(path, cache)`: the session for a profile directory.
fn e_session_from_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    const WHAT: &str = "session.fromPath";
    let path = require_string_arg(args, 0, WHAT)?;
    let cache = session_cache_arg(args, 1);
    let id = electron_state(context)?
        .0
        .borrow_mut()
        .sessions
        .from_path(std::path::Path::new(&path), cache);
    Ok(JsValue::from(id as f64))
}

/// `session.fromPartition(partition, cache)`: the session for a named partition.
fn e_session_from_partition(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    const WHAT: &str = "session.fromPartition";
    let partition = require_string_arg(args, 0, WHAT)?;
    let cache = session_cache_arg(args, 1);
    let id = electron_state(context)?
        .0
        .borrow_mut()
        .sessions
        .from_partition(&partition, cache);
    Ok(JsValue::from(id as f64))
}

/// `session.defaultSession`: the shared default session id.
fn e_session_default(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = electron_state(context)?
        .0
        .borrow()
        .sessions
        .default_session();
    Ok(JsValue::from(id as f64))
}

/// `session.protocol.handle(sessionId, scheme, handler)`: record the scheme
/// in the compat core and keep the JS handler for future dispatch.
fn e_session_protocol_handle(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    const WHAT: &str = "session.protocol.handle";
    let id = session_id_arg(args, context, WHAT)?;
    let scheme = require_string_arg(args, 1, WHAT)?;
    let handler = require_callable_arg(args, 2, WHAT)?;
    let shared = electron_state(context)?;
    let mut state = shared.0.borrow_mut();
    state
        .sessions
        .record_protocol_handler(id, &scheme)
        .map_err(|error| JsNativeError::error().with_message(error.to_string()))?;
    state
        .session_protocol_handlers
        .insert((id, scheme), handler);
    Ok(JsValue::undefined())
}

/// Extract `filter.urls` from a webRequest filter (`undefined`/`null` means
/// no filter: intercept everything).
fn web_request_urls_arg(
    filter: &JsValue,
    context: &mut Context,
    what: &str,
) -> JsResult<Vec<String>> {
    if filter.is_undefined() || filter.is_null() {
        return Ok(Vec::new());
    }
    let json = js_to_json(filter, context).map_err(|_| {
        JsNativeError::typ().with_message(format!("{what} `filter` must be an object"))
    })?;
    let urls = json.get("urls").ok_or_else(|| {
        JsNativeError::typ().with_message(format!("{what} `filter` requires a `urls` array"))
    })?;
    let urls = urls.as_array().ok_or_else(|| {
        JsNativeError::typ().with_message(format!("{what} `filter.urls` must be an array"))
    })?;
    urls.iter()
        .map(|entry| {
            entry.as_str().map(str::to_string).ok_or_else(|| {
                JsNativeError::typ()
                    .with_message(format!("{what} `filter.urls` must be strings"))
                    .into()
            })
        })
        .collect()
}

/// Record one `session.webRequest` listener with its URL filter.
fn record_web_request_rule(
    args: &[JsValue],
    context: &mut Context,
    what: &str,
    kind: &'static str,
) -> JsResult<JsValue> {
    let id = session_id_arg(args, context, what)?;
    let filter = args.get(1).cloned().unwrap_or(JsValue::undefined());
    let urls = web_request_urls_arg(&filter, context, what)?;
    let listener = require_callable_arg(args, 2, what)?;
    if electron_state(context)?
        .0
        .borrow()
        .sessions
        .get(id)
        .is_none()
    {
        return Err(JsNativeError::error()
            .with_message(format!("no session with id {id}"))
            .into());
    }
    electron_state(context)?
        .0
        .borrow_mut()
        .session_web_request
        .entry(id)
        .or_default()
        .push(RecordedWebRequestRule {
            kind,
            urls,
            listener,
        });
    Ok(JsValue::undefined())
}

/// `session.webRequest.onBeforeSendHeaders(sessionId, filter, listener)`.
fn e_session_web_request_on_before_send_headers(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    record_web_request_rule(
        args,
        context,
        "session.webRequest.onBeforeSendHeaders",
        "on-before-send-headers",
    )
}

/// `session.webRequest.onHeadersReceived(sessionId, filter, listener)`.
fn e_session_web_request_on_headers_received(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    record_web_request_rule(
        args,
        context,
        "session.webRequest.onHeadersReceived",
        "on-headers-received",
    )
}

/// `nativeTheme.themeSource` wire names.
fn theme_source_name(source: ThemeSource) -> &'static str {
    match source {
        ThemeSource::System => "system",
        ThemeSource::Light => "light",
        ThemeSource::Dark => "dark",
    }
}

/// Parse a `nativeTheme.themeSource` assignment; Electron accepts only the
/// three wire names.
fn parse_theme_source(raw: &str) -> Option<ThemeSource> {
    match raw {
        "system" => Some(ThemeSource::System),
        "light" => Some(ThemeSource::Light),
        "dark" => Some(ThemeSource::Dark),
        _ => None,
    }
}

/// `nativeTheme.shouldUseDarkColors`.
fn e_native_theme_should_use_dark_colors(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let dark = electron_state(context)?
        .0
        .borrow()
        .native_theme
        .should_use_dark_colors();
    Ok(JsValue::from(dark))
}

/// `nativeTheme.themeSource` getter.
fn e_native_theme_get_source(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let source = electron_state(context)?
        .0
        .borrow()
        .native_theme
        .theme_source();
    Ok(JsValue::from(js_string!(theme_source_name(source))))
}

/// `nativeTheme.themeSource = ...` (unknown names throw like Electron's
/// validation).
fn e_native_theme_set_source(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let raw = require_string_arg(args, 0, "nativeTheme.themeSource")?;
    let source = parse_theme_source(&raw).ok_or_else(|| {
        JsNativeError::typ().with_message(format!(
            "nativeTheme.themeSource must be 'system', 'light', or 'dark', got '{raw}'"
        ))
    })?;
    electron_state(context)?
        .0
        .borrow()
        .native_theme
        .set_theme_source(source);
    Ok(JsValue::undefined())
}

/// `win.webContents.session` backing: the owning session of a window.
fn e_window_get_session_id(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let session = electron_state(context)?
        .0
        .borrow()
        .windows
        .get(id)
        .map(|win| win.options().session)
        .ok_or_else(|| {
            JsError::from(JsNativeError::error().with_message("Object has been destroyed"))
        })?;
    Ok(JsValue::from(session as f64))
}

/// `app.setPath(name, value)`: overrides a core-backed named path.
/// `exe`/`module` are read-only; unbacked and unknown names throw.
fn e_app_set_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = require_string_arg(args, 0, "app.setPath")?;
    let value = require_string_arg(args, 1, "app.setPath")?;
    match electron_path_target(&name).map_err(electron_path_error)? {
        ElectronPathTarget::Core(kind) => {
            electron_state(context)?
                .0
                .borrow_mut()
                .app
                .set_path(kind, PathBuf::from(value));
        }
        ElectronPathTarget::Exe | ElectronPathTarget::ModuleDir => {
            return Err(electron_path_error(format!("Path '{name}' is read-only")));
        }
    }
    Ok(JsValue::undefined())
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

/// Read `webPreferences.preload` from a `BrowserWindow` options object
/// (issue #109). The option is accepted and its path recorded on the compat
/// window for the embedder to execute before renderer scripts; every other
/// `webPreferences` sub-key is accepted and ignored (soft-ignore with the
/// documented note on [`strake_electron_compat::WebPreferences`]).
fn read_web_preferences(
    obj: &JsObject,
    options: &mut BrowserWindowOptions,
    context: &mut Context,
) -> JsResult<()> {
    let prefs = obj.get(js_string!("webPreferences"), context)?;
    let Some(prefs) = prefs.as_object() else {
        return Ok(());
    };
    let preload = prefs.get(js_string!("preload"), context)?;
    if !preload.is_undefined() && !preload.is_null() {
        options.web_preferences.preload = Some(to_rust_string(&preload, context)?);
    }
    // `webPreferences.session`: a Session wrapper from `session.fromPath` /
    // `fromPartition` carries its compat id; anything else keeps the
    // default session (accepted-and-ignored like the other sub-keys).
    let session = prefs.get(js_string!("session"), context)?;
    if let Some(obj) = session.as_object() {
        let raw = obj.get(js_string!("__strakeSessionId"), context)?;
        if !raw.is_undefined() && !raw.is_null() {
            let id = raw.to_number(context)?;
            if id.is_finite() && id >= 0.0 {
                options.session = id as SessionId;
            }
        }
    }
    Ok(())
}

/// `new BrowserWindow(options)`: create the compat-core window. Only
/// `width`/`height`/`show`/`title`/`resizable`/`webPreferences.preload` shape
/// behavior; every other Electron option is accepted and ignored (later
/// slices bind the rest to `strake-shell`).
fn e_window_create(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let shared = electron_state(context)?;
    let mut options = BrowserWindowOptions::default();
    if let Some(obj) = args.first().and_then(|value| value.as_object()) {
        options.width = options_u32(&obj, "width", 800, context)?;
        options.height = options_u32(&obj, "height", 600, context)?;
        options.show = options_bool(&obj, "show", true, context)?;
        options.resizable = options_bool(&obj, "resizable", true, context)?;
        let title = obj.get(js_string!("title"), context)?;
        if !title.is_undefined() && !title.is_null() {
            options.title = to_rust_string(&title, context)?;
        }
        read_web_preferences(&obj, &mut options, context)?;
    }
    let mut state = shared.0.borrow_mut();
    let id = state.windows.create(options);
    state.created_window_ids.push(id);
    Ok(JsValue::from(id as f64))
}

/// `BrowserWindow.getAllWindows()` (issue #107): ids of live windows in
/// ascending creation order; the JS wrapper rehydrates one facade per id, so
/// an empty manager yields an empty array.
fn e_windows_all(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let shared = electron_state(context)?;
    let ids: Vec<JsValue> = shared
        .0
        .borrow()
        .windows
        .live_ids()
        .into_iter()
        .map(|id| JsValue::from(id as f64))
        .collect();
    Ok(JsValue::from(JsArray::from_iter(ids, context)))
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

/// `win.hide()` (issue #155). Unknown windows are ignored, like `show` on
/// a closed window.
fn e_window_hide(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let shared = electron_state(context)?;
    shared.0.borrow_mut().windows.hide(id);
    Ok(JsValue::undefined())
}

/// Electron's "Object has been destroyed" for methods that require a live
/// window. (`webContents.send` deliberately does NOT use this: issue #91
/// mandates fire-and-forget soft failure for unreachable targets.)
fn destroyed_window() -> JsError {
    JsError::from(JsNativeError::error().with_message("Object has been destroyed"))
}

/// `win.setResizable(flag)` (issue #90). Unknown windows are ignored, like
/// `show`/`hide` on a closed window.
fn e_window_set_resizable(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let flag = args.get(1).is_some_and(|value| value.to_boolean());
    let shared = electron_state(context)?;
    shared.0.borrow_mut().windows.set_resizable(id, flag);
    Ok(JsValue::undefined())
}

/// `win.isVisible()` (issue #90). Unknown windows report `false`.
fn e_window_is_visible(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let shared = electron_state(context)?;
    Ok(JsValue::from(shared.0.borrow().windows.is_visible(id)))
}

fn rect_number(options: &JsObject, name: &str, context: &mut Context) -> JsResult<f64> {
    let value = options.get(JsString::from(name), context)?;
    if value.is_undefined() || value.is_null() {
        return Ok(0.0);
    }
    let number = value.to_number(context)?;
    Ok(if number.is_finite() { number } else { 0.0 })
}

/// Read an `Electron.Rectangle` (`{ x, y, width, height }`) from a JS value.
fn js_to_bounds(value: &JsValue, context: &mut Context) -> JsResult<Bounds> {
    let Some(obj) = value.as_object() else {
        return Err(JsError::from(
            JsNativeError::typ().with_message("win.setBounds requires a rectangle object"),
        ));
    };
    Ok(Bounds {
        x: rect_number(&obj, "x", context)? as i32,
        y: rect_number(&obj, "y", context)? as i32,
        width: rect_number(&obj, "width", context)?.max(0.0) as u32,
        height: rect_number(&obj, "height", context)?.max(0.0) as u32,
    })
}

fn bounds_json(bounds: &Bounds) -> serde_json::Value {
    serde_json::json!({
        "x": bounds.x,
        "y": bounds.y,
        "width": bounds.width,
        "height": bounds.height,
    })
}

fn display_json(display: &strake_electron_compat::Display) -> serde_json::Value {
    serde_json::json!({
        "id": display.id,
        "bounds": bounds_json(&display.bounds),
        "workArea": bounds_json(&display.work_area),
        "scaleFactor": display.scale_factor,
    })
}

/// `win.setBounds(rect)` (issue #90). Throws "Object has been destroyed" for
/// unknown windows, like `loadFile`.
fn e_window_set_bounds(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let rect = args.get(1).ok_or_else(|| {
        JsError::from(
            JsNativeError::typ().with_message("win.setBounds requires a rectangle object"),
        )
    })?;
    let bounds = js_to_bounds(rect, context)?;
    let shared = electron_state(context)?;
    if !shared.0.borrow_mut().windows.set_bounds(id, bounds) {
        return Err(destroyed_window());
    }
    Ok(JsValue::undefined())
}

/// `win.getBounds()` (issue #90): `{ x, y, width, height }`.
fn e_window_get_bounds(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let shared = electron_state(context)?;
    let bounds = shared
        .0
        .borrow()
        .windows
        .get_bounds(id)
        .ok_or_else(destroyed_window)?;
    json_to_js(&bounds_json(&bounds), context)
}

/// `win.webContents.getTitle()` (issue #90): the loaded page's `<title>`.
fn e_window_get_title(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let shared = electron_state(context)?;
    let state = shared.0.borrow();
    let title = state
        .windows
        .get(id)
        .ok_or_else(destroyed_window)?
        .web_contents()
        .get_title()
        .to_string();
    Ok(JsValue::from(js_string!(title.as_str())))
}

/// `win.webContents.send(channel, ...args)` (issue #91): queue JSON payloads
/// on the target window for the pump. Unknown/closed windows fail softly
/// (undefined, no throw), matching Electron's fire-and-forget posture. The
/// argument list travels as one JSON array; the pump spreads it back into
/// `(event, ...args)` for renderer `ipcRenderer.on` listeners.
fn e_window_web_contents_send(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let channel = require_string_arg(args.get(1..).unwrap_or(&[]), 0, "win.webContents.send")?;
    let payloads = json_args(args.get(2..).unwrap_or(&[]), context)?;
    let shared = electron_state(context)?;
    shared.0.borrow_mut().windows.queue_web_contents_send(
        id,
        &channel,
        serde_json::Value::Array(payloads),
    );
    Ok(JsValue::undefined())
}

/// `screen.getPrimaryDisplay()` (issue #96). `null` without metrics.
fn e_screen_get_primary_display(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let shared = electron_state(context)?;
    let state = shared.0.borrow();
    match state.screen.get_primary_display() {
        Some(display) => json_to_js(&display_json(display), context),
        None => Ok(JsValue::null()),
    }
}

/// `screen.getAllDisplays()` (issue #96).
fn e_screen_get_all_displays(
    _: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let shared = electron_state(context)?;
    let state = shared.0.borrow();
    let displays: Vec<serde_json::Value> = state
        .screen
        .get_all_displays()
        .iter()
        .map(display_json)
        .collect();
    json_to_js(&serde_json::Value::Array(displays), context)
}

/// `screen.getDisplayMatching(rect)` (issue #96). `null` without metrics.
fn e_screen_get_display_matching(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let bounds = match args.first() {
        Some(rect) => js_to_display_match_rect(rect, context)?,
        None => Bounds::default(),
    };
    let shared = electron_state(context)?;
    let state = shared.0.borrow();
    match state.screen.get_display_matching(&bounds) {
        Some(display) => json_to_js(&display_json(display), context),
        None => Ok(JsValue::null()),
    }
}

/// `screen.getDisplayNearestPoint(point)` (issue #96). `null` without
/// metrics. Only `x`/`y` are read; extra rectangle fields are ignored.
fn e_screen_get_display_nearest_point(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let point = match args.first() {
        Some(value) => js_to_display_match_rect(value, context)?,
        None => Bounds::default(),
    };
    let shared = electron_state(context)?;
    let state = shared.0.borrow();
    match state.screen.get_display_nearest_point(point.x, point.y) {
        Some(display) => json_to_js(&display_json(display), context),
        None => Ok(JsValue::null()),
    }
}

/// `getDisplayMatching` accepts a full rectangle or nothing; unlike
/// `setBounds` a missing/non-object argument means "the zero rect", which
/// still resolves to the primary display when metrics exist.
fn js_to_display_match_rect(value: &JsValue, context: &mut Context) -> JsResult<Bounds> {
    if value.is_undefined() || value.is_null() {
        return Ok(Bounds::default());
    }
    js_to_bounds(value, context)
}

/// `win.on(event, listener)` (issue #84): `closed` fires when this window
/// closes; any other event name is accepted and never fires, matching the
/// `app.on` philosophy for unimplemented surfaces. The JS wrapper returns the
/// window so calls chain like Electron's `EventEmitter.on`.
fn e_window_on(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let event = require_string_arg(args.get(1..).unwrap_or(&[]), 0, "win.on")?;
    let listener = require_callable_arg(args.get(1..).unwrap_or(&[]), 1, "win.on")?;
    let shared = electron_state(context)?;
    if event != "closed" {
        return Ok(JsValue::undefined());
    }
    let mut state = shared.0.borrow_mut();
    if state.windows.get(id).is_none() {
        return Err(JsError::from(
            JsNativeError::error().with_message("Object has been destroyed"),
        ));
    }
    state
        .window_closed_listeners
        .entry(id)
        .or_default()
        .push(listener);
    Ok(JsValue::undefined())
}

/// `webContents.on(event, listener)`: record a renderer-event subscription
/// for a live window (issue #155). Like `win.on`, unknown ids throw
/// Electron's "Object has been destroyed".
fn e_window_web_contents_on(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let rest = args.get(1..).unwrap_or(&[]);
    let event = require_string_arg(rest, 0, "webContents.on")?;
    let listener = require_callable_arg(rest, 1, "webContents.on")?;
    let shared = electron_state(context)?;
    let mut state = shared.0.borrow_mut();
    if state.windows.get(id).is_none() {
        return Err(JsError::from(
            JsNativeError::error().with_message("Object has been destroyed"),
        ));
    }
    state
        .web_contents_listeners
        .entry((id, event))
        .or_default()
        .push(listener);
    Ok(JsValue::undefined())
}

/// `webContents.setWindowOpenHandler(handler)`: record the `window.open`
/// interceptor for a live window (issue #155). Enforcement against actual
/// `window.open` calls is follow-up work; the recording is real.
fn e_window_web_contents_set_window_open_handler(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let handler = require_callable_arg(args, 1, "webContents.setWindowOpenHandler")?;
    let shared = electron_state(context)?;
    let mut state = shared.0.borrow_mut();
    if state.windows.get(id).is_none() {
        return Err(JsError::from(
            JsNativeError::error().with_message("Object has been destroyed"),
        ));
    }
    state.window_open_handlers.insert(id, handler);
    Ok(JsValue::undefined())
}

/// `win.close()`: destroy the window; the last close fires JS
/// `window-all-closed` listeners and runs the compat shutdown flow
/// (Electron's default quit). Closing an unknown or already-destroyed id is
/// an intentional idempotent silent no-op (no throw, no `window-all-closed`,
/// no quit) so double-close is safe; contrast `win.on`, which throws
/// "Object has been destroyed" for such ids like real Electron.
fn e_window_close(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = window_id_arg(args, context)?;
    let shared = electron_state(context)?;
    let (closed, last_closed) = {
        let mut state = shared.0.borrow_mut();
        let closed = state.windows.close(id);
        let last_closed = closed && state.windows.window_count() == 0;
        (closed, last_closed)
    };
    // `closed` first (Electron order), then the app-level last-close flow.
    if closed {
        let listeners = shared
            .0
            .borrow_mut()
            .window_closed_listeners
            .remove(&id)
            .unwrap_or_default();
        call_js_listeners(context, "win 'closed' listener", listeners);
    }
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

/// Best-effort host facts for `require('os')` (issue #155): real values
/// where the standard library can see them, `None`/zero where it cannot.
/// Linux-only files (`/proc/sys/kernel/osrelease`, `/proc/meminfo`,
/// `/proc/uptime`, `/proc/stat`, `/proc/cpuinfo`) are simply tried and
/// skipped elsewhere — no platform branches, no invented numbers. The JS
/// side substitutes marked fallbacks for anything missing.
fn node_os_info() -> serde_json::Value {
    fn trimmed_file(path: &str) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }
    fn mem_kb(field: &str) -> Option<u64> {
        trimmed_file("/proc/meminfo")?.lines().find_map(|line| {
            let (key, rest) = line.split_once(':')?;
            if key.trim() != field {
                return None;
            }
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "arm" => "arm",
        "loongarch64" => "loong64",
        "powerpc" => "ppc",
        "powerpc64" => "ppc64",
        "s390x" => "s390x",
        "riscv64" => "riscv64",
        other => other,
    };
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| trimmed_file("/etc/hostname"));
    let homedir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .filter(|dir| !dir.trim().is_empty());
    let uptime = trimmed_file("/proc/uptime").and_then(|text| {
        text.split_whitespace()
            .next()?
            .parse::<f64>()
            .ok()
            .map(|secs| secs.floor().max(0.0) as u64)
    });
    // Node reports bytes; `/proc/meminfo` reports KiB. `MemAvailable` (what
    // is actually allocatable) answers `freemem`; pre-3.14 kernels without
    // it fall back to `MemFree`.
    let totalmem = mem_kb("MemTotal").map(|kb| kb * 1024).unwrap_or(0);
    let freemem = mem_kb("MemAvailable")
        .or_else(|| mem_kb("MemFree"))
        .map(|kb| kb * 1024)
        .unwrap_or(0);
    // Cumulative CPU times (issue #155): Linux `USER_HZ` is 100 on every
    // supported target, so jiffies become milliseconds with `* 10`.
    let stat_ms = trimmed_file("/proc/stat")
        .and_then(|text| {
            let line = text.lines().find_map(|line| line.strip_prefix("cpu "))?;
            let fields: Vec<u64> = line
                .split_whitespace()
                .filter_map(|field| field.parse::<u64>().ok())
                .collect();
            if fields.len() < 7 {
                return None;
            }
            Some(serde_json::json!({
                "user": fields[0] * 10,
                "nice": fields[1] * 10,
                "sys": fields[2] * 10,
                "idle": (fields[3] + fields[4]) * 10,
                "irq": (fields[5] + fields[6]) * 10,
            }))
        })
        .unwrap_or_else(
            || serde_json::json!({ "user": 0, "nice": 0, "sys": 0, "idle": 0, "irq": 0 }),
        );
    let mut cpus: Vec<serde_json::Value> = trimmed_file("/proc/cpuinfo")
        .map(|text| {
            text.split("\n\n")
                .filter(|block| block.contains("processor"))
                .map(|block| {
                    let field = |name: &str| {
                        block.lines().find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            (key.trim() == name).then(|| value.trim().to_owned())
                        })
                    };
                    let speed = field("cpu MHz")
                        .and_then(|mhz| mhz.parse::<f64>().ok())
                        .map(|mhz| mhz.round().max(0.0) as u64)
                        .unwrap_or(0);
                    serde_json::json!({
                        "model": field("model name").unwrap_or_default(),
                        "speed": speed,
                        "times": stat_ms.clone(),
                    })
                })
                .collect()
        })
        .filter(|entries: &Vec<serde_json::Value>| !entries.is_empty())
        .unwrap_or_default();
    if cpus.is_empty() {
        // No `/proc/cpuinfo` (macOS, Windows): the count stays real via
        // parallelism; model/speed are honestly empty, not invented.
        let count = std::thread::available_parallelism()
            .map(|slots| slots.get())
            .unwrap_or(1);
        cpus = (0..count)
            .map(|_| serde_json::json!({ "model": "", "speed": 0, "times": stat_ms }))
            .collect();
    }
    serde_json::json!({
        "arch": arch,
        "release": trimmed_file("/proc/sys/kernel/osrelease"),
        "hostname": hostname,
        "homedir": homedir,
        "tmpdir": std::env::temp_dir().to_string_lossy(),
        "uptime": uptime,
        "totalmem": totalmem,
        "freemem": freemem,
        "cpus": cpus,
    })
}

/// `__strake_zlib_deflate(data, wrapper)` / `__strake_zlib_inflate(data,
/// wrapper)` (issue #155): real DEFLATE via `flate2` (user-approved) backing
/// `zlib.gzipSync`/`gunzipSync`/`inflateRawSync` and the stream factories.
/// `wrapper` is `"gzip"`, `"zlib"`, or `"raw"`. Corrupt input surfaces as a
/// coded `Z_DATA_ERROR`, never silent garbage.
fn zlib_args(args: &[JsValue], context: &mut Context) -> (Vec<u8>, String) {
    let bytes = args
        .first()
        .and_then(|value| value.as_object())
        .and_then(|obj| JsUint8Array::from_object(obj).ok())
        .and_then(|view| view.to_vec(context).ok())
        .unwrap_or_default();
    let wrapper = args
        .get(1)
        .and_then(|value| value.as_string())
        .map(|text| text.to_std_string_escaped())
        .unwrap_or_default();
    (bytes, wrapper)
}

fn zlib_error(context: &mut Context, syscall: &'static str, error: &std::io::Error) -> JsError {
    crate::node_fs::node_error(
        context,
        "Z_DATA_ERROR",
        -3,
        syscall,
        "",
        format!("{syscall}: {error}"),
    )
}

fn zlib_encode_with<E: std::io::Write>(
    mut encoder: E,
    bytes: &[u8],
    finish: impl FnOnce(E) -> std::io::Result<Vec<u8>>,
) -> std::io::Result<Vec<u8>> {
    encoder.write_all(bytes)?;
    finish(encoder)
}

pub(crate) fn node_zlib_deflate(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (bytes, wrapper) = zlib_args(args, context);
    let out = match wrapper.as_str() {
        "gzip" => zlib_encode_with(
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()),
            &bytes,
            |encoder| encoder.finish(),
        ),
        "zlib" => zlib_encode_with(
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default()),
            &bytes,
            |encoder| encoder.finish(),
        ),
        "raw" => zlib_encode_with(
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default()),
            &bytes,
            |encoder| encoder.finish(),
        ),
        _ => {
            return Err(JsError::from(
                JsNativeError::typ().with_message("wrapper must be 'gzip', 'zlib', or 'raw'"),
            ));
        }
    }
    .map_err(|error| zlib_error(context, "deflate", &error))?;
    Ok(JsValue::from(JsUint8Array::from_iter(out, context)?))
}

pub(crate) fn node_zlib_inflate(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (bytes, wrapper) = zlib_args(args, context);
    let mut decoder: Box<dyn std::io::Read> = match wrapper.as_str() {
        "gzip" => Box::new(flate2::read::GzDecoder::new(&bytes[..])),
        "zlib" => Box::new(flate2::read::ZlibDecoder::new(&bytes[..])),
        "raw" => Box::new(flate2::read::DeflateDecoder::new(&bytes[..])),
        _ => {
            return Err(JsError::from(
                JsNativeError::typ().with_message("wrapper must be 'gzip', 'zlib', or 'raw'"),
            ));
        }
    };
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|error| zlib_error(context, "inflate", &error))?;
    Ok(JsValue::from(JsUint8Array::from_iter(out, context)?))
}

/// `__strake_http_fetch(url, method, headersJson, bodyOrNull, timeoutMs)`
/// (issue #155): one real HTTP transfer via blocking `reqwest` (same crate
/// and TLS backend as `strake-net`), backing `http(s).request`. Blocking is
/// observable-async: the JS side transmits in `Writable._final` and emits
/// `response` on a microtask, so guest code never sees reentrancy. Returns
/// `{ status, statusMessage, headersJson, body }`; transport failures throw
/// coded errors (`ETIMEDOUT`, `ECONNREFUSED`, …).
pub(crate) fn node_http_fetch(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    use std::error::Error as _;
    fn http_fail(context: &mut Context, code: &'static str, errno: i32, detail: String) -> JsError {
        crate::node_fs::node_error(context, code, errno, "request", "", detail)
    }
    // libuv semantics: the negated raw OS error from the source chain, so
    // `ECONNREFUSED` reads -111 on Linux and -61 on macOS with no `cfg`.
    fn chain_errno(error: &reqwest::Error) -> i32 {
        let mut source = error.source();
        while let Some(cause) = source {
            if let Some(io) = cause.downcast_ref::<std::io::Error>() {
                if let Some(raw) = io.raw_os_error() {
                    return -raw;
                }
            }
            source = cause.source();
        }
        -1
    }
    fn fail_send(context: &mut Context, error: reqwest::Error) -> JsError {
        let (code, detail) = if error.is_timeout() {
            ("ETIMEDOUT", format!("request timed out: {error}"))
        } else if error.is_connect() {
            ("ECONNREFUSED", format!("connection refused: {error}"))
        } else if error.is_redirect() {
            (
                "ERR_TOO_MANY_REDIRECTS",
                format!("too many redirects: {error}"),
            )
        } else if error.is_builder() {
            ("ERR_INVALID_ARG_VALUE", format!("invalid request: {error}"))
        } else {
            ("ECONNRESET", format!("request failed: {error}"))
        };
        http_fail(context, code, chain_errno(&error), detail)
    }
    let url = args
        .first()
        .and_then(|value| value.as_string())
        .map(|text| text.to_std_string_escaped())
        .unwrap_or_default();
    let method = args
        .get(1)
        .and_then(|value| value.as_string())
        .map(|text| text.to_std_string_escaped())
        .unwrap_or_else(|| String::from("GET"));
    let headers: Vec<(String, String)> = args
        .get(2)
        .and_then(|value| value.as_string())
        .and_then(|text| serde_json::from_str(text.to_std_string_escaped().as_str()).ok())
        .unwrap_or_default();
    let body: Option<Vec<u8>> = args
        .get(3)
        .and_then(|value| value.as_object())
        .and_then(|obj| JsUint8Array::from_object(obj).ok())
        .and_then(|view| view.to_vec(context).ok())
        .filter(|bytes| !bytes.is_empty());
    let timeout = args
        .get(4)
        .and_then(|value| value.as_number())
        .filter(|ms| *ms > 0.0)
        .map(|ms| std::time::Duration::from_millis(ms as u64));
    let mut builder = reqwest::blocking::Client::builder();
    if let Some(limit) = timeout {
        builder = builder.timeout(limit);
    }
    let client = builder.build().map_err(|error| fail_send(context, error))?;
    let mut request = client
        .request(
            method.parse::<reqwest::Method>().map_err(|_| {
                http_fail(
                    context,
                    "ERR_INVALID_ARG_VALUE",
                    -1,
                    format!("invalid method: {method}"),
                )
            })?,
            url.as_str(),
        )
        .headers({
            let mut map = reqwest::header::HeaderMap::new();
            for (name, value) in &headers {
                let key: reqwest::header::HeaderName = name.parse().map_err(|_| {
                    http_fail(
                        context,
                        "ERR_INVALID_HTTP_TOKEN",
                        -1,
                        format!("invalid header name: {name}"),
                    )
                })?;
                let val: reqwest::header::HeaderValue = value.parse().map_err(|_| {
                    http_fail(
                        context,
                        "ERR_INVALID_CHAR",
                        -1,
                        format!("invalid header value for {name}"),
                    )
                })?;
                map.append(key, val);
            }
            map
        });
    if let Some(bytes) = body {
        request = request.body(bytes);
    }
    let response = request.send().map_err(|error| fail_send(context, error))?;
    let status = response.status();
    let mut header_list = Vec::new();
    for (name, value) in response.headers().iter() {
        header_list.push((
            name.as_str().to_owned(),
            value.to_str().unwrap_or_default().to_owned(),
        ));
    }
    let headers_json = serde_json::to_string(&header_list).unwrap_or_else(|_| String::from("[]"));
    let body = response
        .bytes()
        .map_err(|error| fail_send(context, error))?
        .to_vec();
    let body = JsUint8Array::from_iter(body, context)?;
    let mut init = ObjectInitializer::new(context);
    init.property(
        js_string!("status"),
        f64::from(status.as_u16()),
        Attribute::all(),
    );
    init.property(
        js_string!("statusMessage"),
        JsValue::from(JsString::from(
            status.canonical_reason().unwrap_or_default(),
        )),
        Attribute::all(),
    );
    init.property(
        js_string!("headersJson"),
        JsValue::from(JsString::from(headers_json)),
        Attribute::all(),
    );
    init.property(js_string!("body"), body, Attribute::all());
    Ok(JsValue::from(init.build()))
}

/// `__strake_net_connect(host, port, timeoutMs)` (issue #155): blocking TCP
/// connect backing `net.Socket` port truth (connect-vs-refused, which
/// Joplin's port probe leans on). Returns a handle number stored in host
/// state for `end`/`destroy`; byte transfer rides the later duplex bridge.
/// Refusals, timeouts, and DNS failures throw coded `ECONNREFUSED` /
/// `ETIMEDOUT` / `ENOTFOUND` with libuv-style negated errnos.
pub(crate) fn node_net_connect(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    use std::net::ToSocketAddrs as _;
    fn net_fail(context: &mut Context, code: &'static str, error: &std::io::Error) -> JsError {
        crate::node_fs::node_error(
            context,
            code,
            error.raw_os_error().map(|raw| -raw).unwrap_or(-1),
            "connect",
            "",
            format!("connect: {error}"),
        )
    }
    let host = args
        .first()
        .and_then(|value| value.as_string())
        .map(|text| text.to_std_string_escaped())
        .unwrap_or_default();
    let port = args
        .get(1)
        .and_then(|value| value.as_number())
        .unwrap_or(0.0)
        .clamp(0.0, 65535.0) as u16;
    let timeout = args
        .get(2)
        .and_then(|value| value.as_number())
        .filter(|ms| *ms > 0.0)
        .map(|ms| std::time::Duration::from_millis(ms as u64))
        .unwrap_or_else(|| std::time::Duration::from_secs(10));
    let addr = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|error| net_fail(context, "ENOTFOUND", &error))?
        .next()
        .ok_or_else(|| {
            net_fail(
                context,
                "ENOTFOUND",
                &std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses"),
            )
        })?;
    let stream =
        std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|error| {
            match error.kind() {
                std::io::ErrorKind::ConnectionRefused => net_fail(context, "ECONNREFUSED", &error),
                std::io::ErrorKind::TimedOut => net_fail(context, "ETIMEDOUT", &error),
                _ => net_fail(context, "ECONNRESET", &error),
            }
        })?;
    let shared = electron_state(context)?;
    let mut state = shared.0.borrow_mut();
    let id = state.net_next_socket;
    state.net_next_socket += 1;
    state.net_sockets.insert(id, stream);
    Ok(JsValue::from(id as f64))
}

/// `__strake_net_close(handle)`: drop a connect handle (idempotent).
pub(crate) fn node_net_close(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = args
        .first()
        .and_then(|value| value.as_number())
        .unwrap_or(-1.0) as u64;
    if let Ok(shared) = electron_state(context) {
        shared.0.borrow_mut().net_sockets.remove(&id);
    }
    Ok(JsValue::undefined())
}

/// `__strake_random_bytes(size)`: OS entropy as a `Uint8Array` (issue #155).
/// Backs `crypto.randomBytes`/`randomUUID` so `uuid` seeding matches Node.
/// Runs in main and renderer installs alike (registered by
/// `install_node_standins`). A fill failure surfaces as coded
/// `ERR_SYSTEM_ERROR` — never fake randomness.
pub(crate) fn node_random_bytes(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    // Sizes are validated in JS (`ERR_INVALID_ARG_TYPE`/`ERR_OUT_OF_RANGE`);
    // this clamps defensively so a hostile caller cannot force a huge alloc.
    let size = args
        .first()
        .and_then(|value| value.as_number())
        .map(|size| size.floor().clamp(0.0, 65536.0) as usize)
        .unwrap_or(0);
    let mut bytes = vec![0u8; size];
    getrandom::fill(&mut bytes).map_err(|error| {
        crate::node_fs::node_error(
            context,
            "ERR_SYSTEM_ERROR",
            -1,
            "randomBytes",
            "",
            format!("randomBytes: entropy source failed ({error})"),
        )
    })?;
    Ok(JsValue::from(JsUint8Array::from_iter(bytes, context)?))
}

impl crate::runtime::ScriptRuntime {
    /// Seed `globalThis.__strake_node_info` and evaluate the Node core
    /// stand-ins (issue #108). Runs per context: main and renderer installs
    /// each build their own module objects because contexts must never share
    /// JS objects. `process_type` is the Electron flavor (`browser` for
    /// main, `renderer` for renderers) reported as `process.type` (issue
    /// #155).
    fn install_node_standins(&mut self, process_type: &str) {
        register_primitive(&mut self.context, "__strake_http_fetch", 5, node_http_fetch);
        register_primitive(
            &mut self.context,
            "__strake_net_connect",
            3,
            node_net_connect,
        );
        register_primitive(&mut self.context, "__strake_net_close", 1, node_net_close);
        register_primitive(
            &mut self.context,
            "__strake_random_bytes",
            1,
            node_random_bytes,
        );
        register_primitive(
            &mut self.context,
            "__strake_zlib_deflate",
            2,
            node_zlib_deflate,
        );
        register_primitive(
            &mut self.context,
            "__strake_zlib_inflate",
            2,
            node_zlib_inflate,
        );
        let platform = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            _ => "linux",
        };
        let app_root = std::env::current_dir()
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_else(|_| String::from("/"));
        // Host environment, as in Node (`process.env` inherits the parent
        // environ). `vars` skips non-Unicode entries, keeping this JSON-safe.
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let info = serde_json::json!({
            "platform": platform,
            "versions": {
                "node": "0.0.0-strake",
                "chrome": "0.0.0-strake",
                "electron": "0.0.0-strake",
            },
            "appRoot": app_root,
            "env": env,
            "os": node_os_info(),
            "processType": process_type,
        });
        // The literal above always converts; on failure the bootstrap falls
        // back to its own defaults instead of breaking the install.
        if let Ok(info) = json_to_js(&info, &mut self.context) {
            let _ = self.context.global_object().set(
                js_string!("__strake_node_info"),
                info,
                false,
                &mut self.context,
            );
        }
        self.eval(NODE_STANDIN_BOOTSTRAP_JS, "<strake-node-standins>");
    }

    /// Point module resolution at an app dir (issue #110): `__dirname` and
    /// `process.cwd()` report `root`, so `path.join(__dirname, ...)` resolves
    /// under the app. Runs after an Electron install, which seeds
    /// `__strake_node_info`; without one only `__dirname` is set.
    pub(crate) fn set_node_app_root(&mut self, root: &str) {
        let root_js = JsValue::from(js_string!(root));
        let global = self.context.global_object();
        if let Ok(info) = global.get(js_string!("__strake_node_info"), &mut self.context)
            && let Some(info) = info.as_object()
        {
            let _ = info.set(
                js_string!("appRoot"),
                root_js.clone(),
                false,
                &mut self.context,
            );
        }
        let _ = global.set(js_string!("__dirname"), root_js, false, &mut self.context);
    }

    /// Install the Electron host: primitives, the `require` global, and the
    /// assembled module object (idempotent: reinstalling replaces the host).
    pub(crate) fn install_electron_host(&mut self, shared: &SharedElectronHost) {
        register_primitive(&mut self.context, "require", 1, e_require);
        register_primitive(
            &mut self.context,
            "__strake_electron_windows_all",
            0,
            e_windows_all,
        );
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
            "__strake_electron_app_set_name",
            1,
            e_app_set_name,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_set_as_default_protocol_client",
            1,
            e_app_set_as_default_protocol_client,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_set_app_user_model_id",
            1,
            e_app_set_app_user_model_id,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_protocol_register_schemes_as_privileged",
            1,
            e_protocol_register_schemes_as_privileged,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_session_from_path",
            2,
            e_session_from_path,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_session_from_partition",
            2,
            e_session_from_partition,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_session_default",
            0,
            e_session_default,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_session_protocol_handle",
            3,
            e_session_protocol_handle,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_session_web_request_on_before_send_headers",
            3,
            e_session_web_request_on_before_send_headers,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_session_web_request_on_headers_received",
            3,
            e_session_web_request_on_headers_received,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_get_session_id",
            1,
            e_window_get_session_id,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_native_theme_should_use_dark_colors",
            0,
            e_native_theme_should_use_dark_colors,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_native_theme_get_source",
            0,
            e_native_theme_get_source,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_native_theme_set_source",
            1,
            e_native_theme_set_source,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_get_app_path",
            0,
            e_app_get_app_path,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_get_path",
            1,
            e_app_get_path,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_app_set_path",
            2,
            e_app_set_path,
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
            "__strake_electron_window_hide",
            1,
            e_window_hide,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_close",
            1,
            e_window_close,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_on",
            3,
            e_window_on,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_web_contents_on",
            3,
            e_window_web_contents_on,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_web_contents_set_window_open_handler",
            2,
            e_window_web_contents_set_window_open_handler,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_set_resizable",
            2,
            e_window_set_resizable,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_is_visible",
            1,
            e_window_is_visible,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_set_bounds",
            2,
            e_window_set_bounds,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_get_bounds",
            1,
            e_window_get_bounds,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_get_title",
            1,
            e_window_get_title,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_window_web_contents_send",
            2,
            e_window_web_contents_send,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_screen_get_primary_display",
            0,
            e_screen_get_primary_display,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_screen_get_all_displays",
            0,
            e_screen_get_all_displays,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_screen_get_display_matching",
            1,
            e_screen_get_display_matching,
        );
        register_primitive(
            &mut self.context,
            "__strake_electron_screen_get_display_nearest_point",
            1,
            e_screen_get_display_nearest_point,
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
        // Node core stand-ins (`node:path`, `process`, `url`, `__dirname`):
        // per-context objects for `require` outside `'electron'` (issue #108).
        self.install_node_standins("browser");
        // Main-process contexts expose no DOM globals (issue #155): real
        // Electron main has no `window`/`document`, and entry-point guards
        // (`@sentry/electron`: `typeof window < "u" ? "renderer" : "main"`)
        // take the renderer branch when they exist. Undefined-ing (rather
        // than deleting) works whether the DOM layer installed them as
        // non-configurable or not; main-bundle code reading them unguarded
        // is already broken in real Electron.
        self.eval(
            "globalThis.window = undefined; globalThis.document = undefined;",
            "<strake-electron-main-no-dom>",
        );
        // Real `node:fs` sync shell over the capability-gated native
        // primitives (issue #154): main-process installs only, so renderers
        // keep resolving `fs` to "Cannot find module".
        register_primitive(
            &mut self.context,
            "__strake_fs_exists",
            1,
            crate::node_fs::fs_exists,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_access",
            2,
            crate::node_fs::fs_access,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_mkdir",
            2,
            crate::node_fs::fs_mkdir,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_read",
            1,
            crate::node_fs::fs_read,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_write",
            3,
            crate::node_fs::fs_write,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_stat",
            2,
            crate::node_fs::fs_stat,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_readdir",
            1,
            crate::node_fs::fs_readdir,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_unlink",
            1,
            crate::node_fs::fs_unlink,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_rename",
            2,
            crate::node_fs::fs_rename,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_copy",
            2,
            crate::node_fs::fs_copy,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_realpath",
            1,
            crate::node_fs::fs_realpath,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_readlink",
            1,
            crate::node_fs::fs_readlink,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_utimes",
            3,
            crate::node_fs::fs_utimes,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_open",
            3,
            crate::node_fs::fs_open,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_close",
            1,
            crate::node_fs::fs_close,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_read_fd",
            5,
            crate::node_fs::fs_read_fd,
        );
        register_primitive(
            &mut self.context,
            "__strake_fs_write_fd",
            5,
            crate::node_fs::fs_write_fd,
        );
        self.eval(NODE_FS_BOOTSTRAP_JS, "<strake-node-fs>");

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
    // Read-only main-process window observation for preloads (issue #147):
    // `getAllWindows()` paints the booted count headed and booted alike.
    // Construction and mutation stay main-process-only; facades carry `id`.
    const rendererBrowserWindow = {
        getAllWindows() {
            return globalThis.__strake_electron_windows_all().map((id) => ({ id }));
        },
    };
    globalThis.__strake_electron_renderer_module = { ipcRenderer, BrowserWindow: rendererBrowserWindow };
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
    if specifier == "electron" {
        let shared = electron_state(context)?;
        return shared
            .0
            .borrow()
            .renderer_module
            .clone()
            .map(JsValue::from)
            .ok_or_else(|| {
                JsNativeError::error()
                    .with_message("Electron renderer module not initialised")
                    .into()
            });
    }
    // Preloads and renderer scripts share the pure Node core stand-ins
    // (issue #108, real `fs` stays main-only per issue #154); anything else
    // still throws (native addons are owned by issue #144).
    if let Some(module) = node_standin_module(context, &specifier)? {
        return Ok(module);
    }
    // Renderer parity for the issue #151 file loader: preloads may require
    // sibling files via relative paths or vendored `node_modules`.
    if !is_node_core(&specifier) {
        let shared = electron_state(context)?;
        let app_root = app_root_dir(context);
        if let Some(path) = resolve_commonjs(&specifier, &shared, &app_root) {
            return load_resolved_file(&path, &shared, context);
        }
    }
    Err(cannot_find_module(&specifier))
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
/// listener, invoked by the pump for main-originated `webContents.send`
/// payloads (issue #91).
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
        // Read-only window observation for the renderer `BrowserWindow`
        // namespace (issue #147); creation and mutation stay main-only.
        register_primitive(
            &mut self.context,
            "__strake_electron_windows_all",
            0,
            e_windows_all,
        );
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
        // Preloads run in renderer scope and share the stand-ins (issue #108).
        self.install_node_standins("renderer");

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

    /// Pump queued renderer IPC through main-process handlers (issue #83),
    /// then queued main-to-renderer `webContents.send` payloads into renderer
    /// `ipcRenderer.on` listeners (issue #91): each `invoke` runs its
    /// `ipcMain.handle` callback in this (main) context and settles the
    /// renderer promise; each `send` fans out to `ipcMain.on` listeners; each
    /// main-send invokes the renderer's channel listeners as
    /// `(event, ...args)` in the renderer context. Returns the number of
    /// pumped calls. All shared borrows are statement-scoped takes, so
    /// re-entrant JS (handlers calling back into Electron) cannot trip the
    /// `RefCell`s.
    ///
    /// Routing note: main-sends queue per target window in the compat core,
    /// but renderers are not bound to windows yet, so the pump fans each
    /// payload out to the attached renderer's channel listeners. Per-window
    /// renderer binding rides with the multi-window transport (issue #15).
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

        // Main-to-renderer leg (issue #91): drain every window's outbox and
        // invoke the renderer's channel listeners as `(event, ...args)`. The
        // queued payload is the `...args` array (see
        // `e_window_web_contents_send`); a non-array payload (only possible
        // from Rust callers) arrives as a single argument.
        let main_sends = shared.0.borrow_mut().windows.drain_web_contents_sends();
        for (_window_id, send) in main_sends {
            pumped += 1;
            let listeners = shared
                .0
                .borrow()
                .renderer_listeners
                .get(&send.channel)
                .cloned()
                .unwrap_or_default();
            if listeners.is_empty() {
                continue;
            }
            let mut call_args = Vec::new();
            call_args.push(JsValue::from(
                ObjectInitializer::new(&mut renderer.context).build(),
            ));
            let payloads = match &send.payload {
                serde_json::Value::Array(items) => items.clone(),
                single => vec![single.clone()],
            };
            for arg in &payloads {
                match json_to_js(arg, &mut renderer.context) {
                    Ok(value) => call_args.push(value),
                    Err(error) => {
                        record_callback_error(
                            &mut renderer.context,
                            "webContents.send argument",
                            &error,
                        );
                        call_args.push(JsValue::null());
                    }
                }
            }
            for listener in listeners {
                if let Err(error) =
                    listener.call(&JsValue::undefined(), &call_args, &mut renderer.context)
                {
                    record_callback_error(&mut renderer.context, "ipcRenderer.on listener", &error);
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
