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
use boa_engine::object::builtins::{JsArray, JsFunction, JsPromise};
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source,
    js_string,
};
use strake_electron_compat::{
    App, Bounds, BrowserWindowOptions, Clipboard, Enforcer, NotificationCenter,
    NotificationRequest, PermissionManifest, PowerHub, PowerSaveBlockerKind, SafeStorage, Screen,
    WindowManager,
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
/// (`{ platform, versions, appRoot, env }`, installed natively per context):
/// `platform` follows Node's names (`darwin`/`win32`/`linux`), `versions`
/// carries Strake-marked strings until a real Node ABI exists, and
/// `__dirname` defaults to the app root (per-file module semantics need the
/// issue #16 loader; the `#110` runner sets the app root before eval).
const NODE_STANDIN_BOOTSTRAP_JS: &str = r#"
(function () {
    const info = globalThis.__strake_node_info || {};
    const versions = info.versions || {};
    const split = (p) => String(p).split("/").filter((seg) => seg.length > 0);
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
        const absolute = flat.startsWith("/");
        const joined = (absolute ? "/" : "") + normalizeSegs(split(flat), absolute).join("/");
        return joined === "" ? "." : joined;
    };
    const dirname = (p) => {
        const s = String(p);
        const idx = s.replace(/\/+$/, "").lastIndexOf("/");
        if (idx < 0) return ".";
        if (idx === 0) return "/";
        return s.slice(0, idx);
    };
    const basename = (p, ext) => {
        const s = String(p).replace(/\/+$/, "").split("/").pop() || "";
        if (ext && s.endsWith(ext)) return s.slice(0, s.length - ext.length);
        return s;
    };
    const pathModule = {
        join,
        normalize: (p) => join(String(p)),
        dirname,
        basename,
        isAbsolute: (p) => String(p).startsWith("/"),
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
    // `stream` `.Stream` base (issue #154): an `EventEmitter` subclass with
    // `pipe`, which is all `graceful-fs` needs at load; `Readable`/`Writable`
    // file classes live on the `fs` shell where the native primitives are.
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
        return { Stream };
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
        return { format, debuglog, inherits };
    })();
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
        "node:stream": streamModule,
        stream: streamModule,
        "node:util": utilModule,
        util: utilModule,
    };
    // Node's global `Buffer` (raw reads in Joplin startup use it bare).
    if (typeof globalThis.Buffer === "undefined") {
        globalThis.Buffer = bufferModule.Buffer;
    }
    if (typeof globalThis.process === "undefined") {
        globalThis.process = processModule;
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
/// accepted and ignored), unlink/rename/copyFile, realpath, `constants`,
/// and working `ReadStream`/`WriteStream` classes with `create*` factories.
/// Async (`fs.promises`, callbacks) and watchers stay out of scope.
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
    class ReadStream extends stream.Stream {
        constructor(path, options) {
            super();
            this.path = toPath(path);
            queueMicrotask(() => this.open());
        }
        open() {
            let bytes;
            try {
                bytes = __strake_fs_read(this.path);
            } catch (error) {
                this.emit("error", error);
                return;
            }
            this.emit("open", 0);
            const CHUNK = 64 * 1024;
            for (let offset = 0; offset < bytes.length; offset += CHUNK) {
                this.emit("data", Buffer.from(bytes.subarray(offset, offset + CHUNK)));
            }
            this.emit("end");
            this.emit("close");
        }
        close(callback) {
            this.emit("close");
            if (typeof callback === "function") callback();
        }
    }
    class WriteStream extends stream.Stream {
        constructor(path, options) {
            super();
            this.path = toPath(path);
            this._chunks = [];
            this._ended = false;
        }
        write(chunk, encoding, callback) {
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
        }
        end(chunk, encoding, callback) {
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
            this.emit("finish");
            done();
            this.emit("close");
        }
        close(callback) {
            this.emit("close");
            if (typeof callback === "function") callback();
        }
    }
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
        createReadStream(path, options) {
            return new ReadStream(path, options);
        },
        createWriteStream(path, options) {
            return new WriteStream(path, options);
        },
    };
    table["node:fs"] = fs;
    table["fs"] = fs;
})();
"#;

/// Mutable Electron main-process state shared between the native primitives
/// (which run inside JS calls) and the [`ElectronHost`] handle held by the
/// embedder. Single-threaded by construction (Boa contexts are `!Send`).
struct ElectronHostState {
    app: App,
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
                windows: WindowManager::new(),
                screen: Screen::default(),
                module: None,
                app_listeners: HashMap::new(),
                when_ready_resolvers: Vec::new(),
                ipc_handlers: HashMap::new(),
                ipc_listeners: HashMap::new(),
                created_window_ids: Vec::new(),
                window_closed_listeners: HashMap::new(),
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
                module_cache: HashMap::new(),
                require_stack: Vec::new(),
                permissions: Enforcer::new(PermissionManifest::default()),
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
        drop(state);
        let snapshot = Self::new("", "");
        {
            let mut fresh = snapshot.shared.0.borrow_mut();
            fresh.app = app;
            fresh.windows = windows;
            fresh.screen = screen;
            fresh.permissions = permissions;
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
        let wrapper = context.eval(Source::from_bytes(&wrapped))?;
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

impl crate::runtime::ScriptRuntime {
    /// Seed `globalThis.__strake_node_info` and evaluate the Node core
    /// stand-ins (issue #108). Runs per context: main and renderer installs
    /// each build their own module objects because contexts must never share
    /// JS objects.
    fn install_node_standins(&mut self) {
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
            "__strake_electron_window_on",
            3,
            e_window_on,
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
        self.install_node_standins();
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
        self.install_node_standins();

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
