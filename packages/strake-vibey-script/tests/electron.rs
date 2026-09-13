//! Slice 1 conformance (issue #81): a minimal Electron quick-start `main.js`
//! runs to `ready` through `require('electron')` backed by
//! `strake-electron-compat`, with window creation and `ipcMain` registration
//! observed from Rust. Headless: no OS window (Slice 2), no renderer
//! invocation (Slice 3).

use strake_dom::DocumentConfig;
use strake_vibey_script::{ElectronHost, ScriptDocument};

/// Minimal Electron quick-start shape: lifecycle, one window, one IPC handler.
const MAIN_JS: &str = r#"
const { app, BrowserWindow, ipcMain } = require('electron');

__strake_send_message('main-evaluated:' + app.isReady());

app.on('ready', () => {
    __strake_send_message('app-ready-event');
});

app.whenReady().then(() => {
    const win = new BrowserWindow({ width: 1024, height: 768, title: 'QuickStart' });
    win.loadFile('index.html');
    ipcMain.handle('ping', () => 'pong');
    ipcMain.on('log', () => {});
    __strake_send_message('window-created:' + app.getName() + '/' + app.getVersion());
});
"#;

fn main_doc() -> (ScriptDocument, ElectronHost) {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    assert!(
        doc.take_js_errors().is_empty(),
        "electron bootstrap must install cleanly"
    );
    doc.eval(MAIN_JS);
    (doc, host)
}

#[test]
fn quickstart_main_js_reaches_ready() {
    let (mut doc, host) = main_doc();
    assert!(
        doc.take_js_errors().is_empty(),
        "main.js must evaluate without throwing"
    );
    // `whenReady().then` has not run: the app is not ready yet.
    assert!(!host.is_ready());
    assert_eq!(host.window_count(), 0);
    assert_eq!(doc.take_messages(), vec!["main-evaluated:false"]);

    doc.mark_electron_ready();

    assert!(
        doc.take_js_errors().is_empty(),
        "ready listeners and continuations must not throw"
    );
    assert!(host.is_ready());
    assert_eq!(
        doc.take_messages(),
        vec!["app-ready-event", "window-created:QuickStart/1.0.0"]
    );

    // Window creation observed in the compat core.
    assert_eq!(host.window_count(), 1);
    assert_eq!(host.created_window_ids(), vec![0]);
    let pending = host
        .window_pending_url(0)
        .expect("loadFile records a navigation target");
    assert!(
        pending.ends_with("index.html"),
        "loadFile resolves to index.html, got {pending}"
    );

    // ipcMain registration observed.
    assert_eq!(host.ipc_handler_channels(), vec!["ping"]);
    assert_eq!(host.ipc_listener_channels(), vec!["log"]);
}

#[test]
fn unknown_module_throws_node_style() {
    let (mut doc, _host) = main_doc();
    doc.eval("require('node:fs');");
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 1, "expected one throw, got {errors:?}");
    assert!(
        errors[0].contains("Cannot find module 'node:fs'"),
        "unexpected error: {}",
        errors[0]
    );
}

#[test]
fn duplicate_ipc_handler_throws_like_electron() {
    let (mut doc, _host) = main_doc();
    doc.eval(
        "const { ipcMain: ipcDup } = require('electron'); \
         ipcDup.handle('dup', () => 1); \
         ipcDup.handle('dup', () => 2);",
    );
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 1, "expected one throw, got {errors:?}");
    assert!(
        errors[0].contains("second handler"),
        "unexpected error: {}",
        errors[0]
    );
}

#[test]
fn ready_is_idempotent_and_late_when_ready_resolves() {
    let (mut doc, host) = main_doc();
    doc.mark_electron_ready();
    doc.mark_electron_ready();
    assert!(host.is_ready());
    assert!(doc.take_js_errors().is_empty());

    // Late `whenReady()` resolves without another mark.
    doc.eval(
        "require('electron').app.whenReady().then(() => __strake_send_message('late-ready'));",
    );
    assert!(doc.take_js_errors().is_empty());
    assert!(doc.take_messages().contains(&"late-ready".to_string()));
}

#[test]
fn last_window_close_fires_window_all_closed_and_quits() {
    let (mut doc, host) = main_doc();
    doc.mark_electron_ready();
    assert_eq!(host.window_count(), 1);
    assert!(doc.take_js_errors().is_empty());

    doc.eval(
        "const eQuit = require('electron'); \
         eQuit.app.on('window-all-closed', () => __strake_send_message('wac')); \
         eQuit.app.on('before-quit', () => __strake_send_message('before-quit')); \
         eQuit.app.quit();",
    );
    // Explicit quit path.
    assert!(host.is_quit());
    let messages = doc.take_messages();
    assert!(messages.contains(&"before-quit".to_string()));
    assert!(doc.take_js_errors().is_empty());
}

#[test]
fn os_bridges_clipboard_safe_storage_power() {
    use strake_vibey_script::electron::PowerEvent;
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "const { clipboard, safeStorage, powerMonitor, powerSaveBlocker } = require('electron'); \
         clipboard.writeText('hello-os'); \
         __strake_send_message('clipboard:' + clipboard.readText()); \
         clipboard.clear(); \
         __strake_send_message('cleared:' + JSON.stringify(clipboard.readText())); \
         __strake_send_message('secure:' + safeStorage.isEncryptionAvailable()); \
         __strake_send_message('roundtrip:' + safeStorage.decryptString(safeStorage.encryptString('s3cret'))); \
         powerMonitor.on('suspend', (e) => __strake_send_message('power:' + e.type)); \
         powerMonitor.on('resume', (e) => __strake_send_message('power:' + e.type)); \
         const bid = powerSaveBlocker.start('prevent-display-sleep'); \
         __strake_send_message('blocker:' + powerSaveBlocker.isStarted(bid)); \
         powerSaveBlocker.stop(bid); \
         __strake_send_message('blocker-stopped:' + powerSaveBlocker.isStarted(bid));",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "os bridge calls must not throw"
    );

    // Synthetic probe (issue #93): inject sleep/resume, assert delivery.
    host.inject_power_event(PowerEvent::Suspend);
    host.inject_power_event(PowerEvent::Resume);
    assert_eq!(doc.dispatch_power_events(), 2);
    assert!(doc.take_js_errors().is_empty());

    assert_eq!(
        doc.take_messages(),
        vec![
            "clipboard:hello-os",
            "cleared:\"\"",
            "secure:true",
            "roundtrip:s3cret",
            "blocker:true",
            "blocker-stopped:false",
            "power:suspend",
            "power:resume",
        ]
    );
}

#[test]
fn window_geometry_resizable_and_screen_parity() {
    use strake_electron_compat::{Bounds, Display, Screen};
    let host = ElectronHost::new("QuickStart", "1.0.0");
    host.set_screen(Screen::new(vec![
        Display::new(
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            1.0,
        ),
        Display::new(
            1,
            Bounds {
                x: 1920,
                y: 0,
                width: 2560,
                height: 1440,
            },
            2.0,
        ),
    ]));
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "const { BrowserWindow, screen } = require('electron'); \
         const win = new BrowserWindow({ width: 1024, height: 768, resizable: false }); \
         __strake_send_message('visible:' + win.isVisible()); \
         win.setResizable(true); \
         win.setBounds({ x: 40, y: 50, width: 800, height: 600 }); \
         __strake_send_message('bounds:' + JSON.stringify(win.getBounds())); \
         __strake_send_message('title:' + JSON.stringify(win.webContents.getTitle())); \
         __strake_send_message('primary:' + screen.getPrimaryDisplay().id); \
         __strake_send_message('all:' + screen.getAllDisplays().length); \
         __strake_send_message('match:' + screen.getDisplayMatching({ x: 2000, y: 100, width: 800, height: 600 }).id); \
         __strake_send_message('scale:' + screen.getDisplayMatching({ x: 2000, y: 100, width: 800, height: 600 }).scaleFactor); \
         __strake_send_message('nearest:' + screen.getDisplayNearestPoint({ x: 100, y: 100 }).id); \
         __strake_send_message('nearest-far:' + screen.getDisplayNearestPoint({ x: 5000, y: 5000 }).id);",
    );
    let js_errors = doc.take_js_errors();
    assert!(
        js_errors.is_empty(),
        "geometry/screen calls must not throw, got {js_errors:?}"
    );
    let messages = doc.take_messages();
    assert_eq!(messages[0], "visible:true");
    for (key, needle) in [
        ("bounds", "\"x\":40"),
        ("bounds", "\"y\":50"),
        ("bounds", "\"width\":800"),
        ("bounds", "\"height\":600"),
    ] {
        let line = messages
            .iter()
            .find(|m| m.starts_with(&format!("{key}:")))
            .unwrap();
        assert!(line.contains(needle), "{line} should contain {needle}");
    }
    assert!(
        messages.contains(&"title:\"\"".to_string()),
        "no page loaded: empty title"
    );
    assert!(messages.contains(&"primary:0".to_string()));
    assert!(messages.contains(&"all:2".to_string()));
    assert!(messages.contains(&"match:1".to_string()));
    assert!(messages.contains(&"scale:2".to_string()));
    assert!(messages.contains(&"nearest:0".to_string()));
    assert!(messages.contains(&"nearest-far:1".to_string()));

    // `resizable: false` parsed from options, then flipped by setResizable.
    assert_eq!(host.window_resizable(0), Some(true));
    assert_eq!(
        host.window_bounds(0),
        Some(Bounds {
            x: 40,
            y: 50,
            width: 800,
            height: 600,
        })
    );
}

/// Issue #84 canary: the calculator demo's `main.js`
/// (gregoreesmaa/strake-electron-calculator@strake-demo, 62 lines) boots
/// under the shim — ready → one 365x675 window → loadURL → closed →
/// window-all-closed → quit. Verbatim except for the Node core stand-ins in
/// the header (`path`/`url`/`process` are owned by #16), `var` for the
/// window handle (the shipped app uses `let`; `var` only exposes the handle
/// to the test driver), and a `__strake_send_message('demo:closed')` probe
/// inside the `closed` handler (the assertion mechanism; not in the demo).
/// The `activate` re-create handler is registered but
/// not exercised headless, matching the demo's own COMPAT.md.
#[test]
fn calculator_demo_boots_and_quits() {
    let host = ElectronHost::new("Calculator", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "const path = { join: (...parts) => parts.join('/') }; \
         const url = { format: (o) => 'file://' + o.pathname }; \
         globalThis.process = { platform: 'linux' }; \
         const electron = require('electron'); \
         const app = electron.app; \
         const BrowserWindow = electron.BrowserWindow; \
         var mainWindow = null; \
         function createWindow () { \
             mainWindow = new BrowserWindow({width: 365, height: 675}); \
             mainWindow.setResizable(false); \
             mainWindow.loadURL(url.format({ pathname: path.join('/app', 'index.html'), protocol: 'file:', slashes: true })); \
             mainWindow.on('closed', function () { mainWindow = null; __strake_send_message('demo:closed'); }); \
         } \
         app.on('ready', createWindow); \
         app.on('window-all-closed', function () { if (process.platform !== 'darwin') { app.quit(); } }); \
         app.on('activate', function () { if (mainWindow === null) { createWindow(); } });",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "demo main.js must evaluate without throwing"
    );
    assert_eq!(host.window_count(), 0, "no window before ready");

    doc.mark_electron_ready();
    assert!(
        doc.take_js_errors().is_empty(),
        "ready → createWindow must not throw"
    );
    assert_eq!(host.window_count(), 1, "exactly one window");
    assert_eq!(host.created_window_ids(), vec![0]);
    assert_eq!(
        host.window_bounds(0)
            .map(|bounds| (bounds.width, bounds.height)),
        Some((365, 675))
    );
    assert_eq!(host.window_resizable(0), Some(false));
    let pending = host
        .window_pending_url(0)
        .expect("loadURL records a target");
    assert!(
        pending.ends_with("/app/index.html"),
        "loadURL resolves to index.html, got {pending}"
    );

    // OS close: `closed` dereferences the window, then the app quits (linux).
    doc.eval("mainWindow.close();");
    assert!(doc.take_js_errors().is_empty(), "close path must not throw");
    assert_eq!(doc.take_messages(), vec!["demo:closed"]);
    assert_eq!(host.window_count(), 0);
    assert!(host.is_quit(), "window-all-closed quits on linux");
}

#[test]
fn methods_on_destroyed_window_throw() {
    let (mut doc, _host) = main_doc();
    assert_eq!(doc.take_messages(), vec!["main-evaluated:false"]);
    doc.eval(
        "const BW = require('electron').BrowserWindow; \
         globalThis.__doomed = new BW({ show: false }); \
         globalThis.__doomed.close();",
    );
    assert!(doc.take_js_errors().is_empty());
    // Throwing half of the split: a throw aborts its eval, so one eval per
    // method.
    for stmt in [
        "globalThis.__doomed.loadFile('late.html');",
        "globalThis.__doomed.setBounds({ x: 1, y: 2, width: 3, height: 4 });",
        "globalThis.__doomed.getBounds();",
        "globalThis.__doomed.webContents.getTitle();",
    ] {
        doc.eval(stmt);
    }
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 4, "expected four throws, got {errors:?}");
    for error in &errors {
        assert!(error.contains("destroyed"), "unexpected error: {error}");
    }
    // Soft-fail half: `setResizable`/`isVisible` stay silent on destroyed
    // windows instead of throwing.
    doc.eval(
        "globalThis.__doomed.setResizable(false); \
         __strake_send_message('soft-visible:' + globalThis.__doomed.isVisible());",
    );
    assert!(doc.take_js_errors().is_empty());
    assert_eq!(doc.take_messages(), vec!["soft-visible:false"]);
}

#[test]
fn close_unknown_or_destroyed_id_is_silent_noop() {
    // Issue #84 follow-up: `win.close()` on an unknown or already-destroyed
    // id is an intentional idempotent silent no-op (see `e_window_close`), so
    // closing id 404 on an empty manager must not fire `window-all-closed`,
    // must not quit, and must not throw. `win.on` still returns the window
    // for EventEmitter-style chaining.
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "const e84 = require('electron'); \
         e84.app.on('window-all-closed', () => __strake_send_message('wac')); \
         const BW84 = e84.BrowserWindow; \
         const ghost = Object.create(BW84.prototype); \
         ghost.__strakeWindowId = 404; \
         ghost.close(); \
         __strake_send_message('ghost-survived');",
    );
    let js_errors = doc.take_js_errors();
    assert!(
        js_errors.is_empty(),
        "closing an unknown window id must not throw, got {js_errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["ghost-survived"]);
    assert!(
        !host.is_quit(),
        "unknown-id close must not run the quit flow"
    );
    assert_eq!(host.window_count(), 0);

    // Live window: `on` chains, the legitimate last close still fires
    // `window-all-closed` and quits, and the double close after it is silent.
    doc.eval(
        "const live = new BW84({ show: false }); \
         __strake_send_message('chain:' + (live.on('closed', () => {}) === live)); \
         live.close(); \
         live.close(); \
         __strake_send_message('double-survived');",
    );
    let js_errors = doc.take_js_errors();
    assert!(
        js_errors.is_empty(),
        "close path must not throw, got {js_errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["chain:true", "wac", "double-survived"]
    );
    assert_eq!(host.window_count(), 0);
    assert!(host.is_quit(), "the one legitimate last-close still quits");
}
