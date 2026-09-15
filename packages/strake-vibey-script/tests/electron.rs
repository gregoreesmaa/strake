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
    // Issue #154 serves `node:fs` for real now, so the unknown-module probe
    // uses a core that stays unimplemented (`node:worker_threads`).
    doc.eval("require('node:worker_threads');");
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 1, "expected one throw, got {errors:?}");
    assert!(
        errors[0].contains("Cannot find module 'node:worker_threads'"),
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

/// Issue #108: Node core stand-ins resolve headlessly with documented
/// Strake values. `node:fs` (owned by issue #16) still throws.
#[test]
fn node_core_standins_resolve() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "const path = require('node:path'); \
         __strake_send_message('join:' + path.join('/app', 'preload.js')); \
         __strake_send_message('join-rel:' + path.join('a', 'b', '..', 'c')); \
         __strake_send_message('dir:' + path.dirname('/app/preload.js')); \
         __strake_send_message('base:' + path.basename('/app/preload.js')); \
         __strake_send_message('abs:' + path.isAbsolute('/x')); \
         __strake_send_message('sep:' + path.sep); \
         const url = require('node:url'); \
         __strake_send_message('fmt:' + url.format({ pathname: '/app/index.html', protocol: 'file:', slashes: true })); \
         __strake_send_message('platform:' + process.platform); \
         __strake_send_message('versions:' + [process.versions.node, process.versions.chrome, process.versions.electron].every((v) => typeof v === 'string' && v.length > 0)); \
         __strake_send_message('dirname-global:' + (typeof __dirname === 'string')); \
         __strake_send_message('process-require:' + (require('node:process') === process)); \
         __strake_send_message('path-alias:' + (require('path') === path)); \
         __strake_send_message('url-alias:' + (require('url') === url));",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "node stand-ins must not throw"
    );
    let expected_platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        _ => "linux",
    };
    let messages = doc.take_messages();
    assert_eq!(messages.len(), 13, "one line per probe, got {messages:?}");
    assert_eq!(
        &messages[..7],
        [
            "join:/app/preload.js",
            "join-rel:a/c",
            "dir:/app",
            "base:preload.js",
            "abs:true",
            "sep:/",
            "fmt:file:///app/index.html",
        ]
    );
    assert_eq!(messages[7], format!("platform:{expected_platform}"));
    assert_eq!(
        &messages[8..],
        [
            "versions:true",
            "dirname-global:true",
            "process-require:true",
            "path-alias:true",
            "url-alias:true",
        ]
    );
}

/// Issue #107: `BrowserWindow.getAllWindows()` returns one facade per live
/// window (empty when none), reflecting create/close through `install_electron`.
#[test]
fn get_all_windows_reflects_lifecycle() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "const { BrowserWindow } = require('electron'); \
         __strake_send_message('empty:' + BrowserWindow.getAllWindows().length); \
         const wA = new BrowserWindow({ show: false }); \
         const wB = new BrowserWindow({ show: false }); \
         const all = BrowserWindow.getAllWindows(); \
         __strake_send_message('count:' + all.length); \
         __strake_send_message('instanceof:' + (all[0] instanceof BrowserWindow)); \
         __strake_send_message('ids:' + all.map((w) => w.__strakeWindowId).join(',')); \
         all[0].close(); \
         __strake_send_message('after:' + BrowserWindow.getAllWindows().length);",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "getAllWindows lifecycle must not throw"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "empty:0",
            "count:2",
            "instanceof:true",
            "ids:0,1",
            "after:1",
        ]
    );
    assert_eq!(host.live_window_ids(), vec![1]);
    doc.eval(
        "require('electron').BrowserWindow.getAllWindows()[0].close(); \
         __strake_send_message('final:' + require('electron').BrowserWindow.getAllWindows().length);",
    );
    assert!(doc.take_js_errors().is_empty());
    assert_eq!(doc.take_messages(), vec!["final:0"]);
    assert!(host.live_window_ids().is_empty());
}

/// Issue #109: `webPreferences` is accepted on window creation (no throw),
/// `preload` is recorded for the embedder queue, unknown sub-keys soft-ignore.
#[test]
fn web_preferences_preload_recorded_unknown_keys_ignored() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "const { BrowserWindow } = require('electron'); \
         const w = new BrowserWindow({ show: false, webPreferences: { preload: '/app/preload.js', sandbox: true, unknownFutureKey: {} } }); \
         __strake_send_message('created:' + (typeof w.__strakeWindowId === 'number'));",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "webPreferences must be accepted without throwing"
    );
    assert_eq!(doc.take_messages(), vec!["created:true"]);
    let preload = host.window_preload(0).expect("preload path is recorded");
    assert!(
        preload.ends_with("preload.js"),
        "recorded preload path, got {preload}"
    );
    assert_eq!(
        host.pending_preloads(),
        vec![(0, preload)],
        "embedder drain queue carries the preload"
    );
}

/// Verbatim `main.js` from gregoreesmaa/strake-minimal-repro@main
/// (commit 314e53a253c808b812ae9eb13703c9d88026e578): the standard
/// electron-quick-start shape. No stubs: `node:path`, `process`, and
/// `__dirname` resolve through the issue #108 stand-ins.
const MINIMAL_REPRO_MAIN_JS: &str = r#"
const { app, BrowserWindow } = require('electron')
const path = require('node:path')

function createWindow () {
  // Create the browser window.
  const mainWindow = new BrowserWindow({
    width: 800,
    height: 600,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js')
    }
  })

  // and load the index.html of the app.
  mainWindow.loadFile('index.html')

  // Open the DevTools.
  // mainWindow.webContents.openDevTools()
}

// This method will be called when Electron has finished
// initialization and is ready to create browser windows.
// Some APIs can only be used after this event occurs.
app.whenReady().then(() => {
  createWindow()

  app.on('activate', function () {
    // On macOS it's common to re-create a window in the app when the
    // dock icon is clicked and there are no other windows open.
    if (BrowserWindow.getAllWindows().length === 0) createWindow()
  })
})

// Quit when all windows are closed, except on macOS. There, it's common
// for applications and their menu bar to stay active until the user quits
// explicitly with Cmd + Q.
app.on('window-all-closed', function () {
  if (process.platform !== 'darwin') app.quit()
})

// In this file you can include the rest of your app's specific main process
// code. You can also put them in separate files and require them here.
"#;

/// Issue #111: the minimal-repro boot canary. Ready → one 800x600 window →
/// `loadFile('index.html')` recorded → zero JS errors. Preload/DOM behavior
/// is pinned by `minimal_repro_preload_fills_version_spans`, not here.
#[test]
fn minimal_repro_main_boots() {
    let host = ElectronHost::new("minimal-repro", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    assert!(
        doc.take_js_errors().is_empty(),
        "electron bootstrap must install cleanly"
    );
    doc.eval(MINIMAL_REPRO_MAIN_JS);
    assert!(
        doc.take_js_errors().is_empty(),
        "verbatim minimal-repro main.js must evaluate without throwing"
    );
    assert_eq!(host.window_count(), 0, "no window before ready");

    doc.mark_electron_ready();
    assert!(
        doc.take_js_errors().is_empty(),
        "ready → createWindow must not throw"
    );
    assert_eq!(host.window_count(), 1, "exactly one 800x600 window");
    assert_eq!(host.created_window_ids(), vec![0]);
    assert_eq!(
        host.window_bounds(0).map(|b| (b.width, b.height)),
        Some((800, 600))
    );
    let pending = host
        .window_pending_url(0)
        .expect("loadFile records a navigation target");
    assert!(
        pending.ends_with("index.html"),
        "loadFile resolves to index.html, got {pending}"
    );
    // Issue #107 acceptance through the real boot path.
    assert_eq!(host.live_window_ids(), vec![0]);
    doc.eval(
        "__strake_send_message('all:' + require('electron').BrowserWindow.getAllWindows().length);",
    );
    assert!(doc.take_js_errors().is_empty());
    // Issue #109 acceptance through the real boot path.
    let preload = host.window_preload(0).expect("preload recorded");
    assert!(
        preload.ends_with("preload.js"),
        "preload joins __dirname, got {preload}"
    );
    assert_eq!(doc.take_messages(), vec!["all:1"]);

    // `activate` re-create path: close the window, then re-run the guard
    // expression from main.js — it must observe zero windows.
    doc.eval(
        "require('electron').BrowserWindow.getAllWindows()[0].close(); \
         __strake_send_message('activate-sees:' + require('electron').BrowserWindow.getAllWindows().length);",
    );
    assert!(doc.take_js_errors().is_empty());
    assert_eq!(doc.take_messages(), vec!["activate-sees:0"]);
    assert_eq!(host.window_count(), 0);
}

/// Verbatim `preload.js` from gregoreesmaa/strake-minimal-repro@main
/// (commit 314e53a253c808b812ae9eb13703c9d88026e578).
const MINIMAL_REPRO_PRELOAD_JS: &str = r#"
window.addEventListener('DOMContentLoaded', () => {
  const replaceText = (selector, text) => {
    const element = document.getElementById(selector)
    if (element) element.innerText = text
  }

  for (const type of ['chrome', 'node', 'electron']) {
    replaceText(`${type}-version`, process.versions[type])
  }
})
"#;

/// Issue #109: the verbatim preload executes in renderer scope after document
/// creation, before page scripts, filling the version spans on
/// `DOMContentLoaded`. The spans are empty before `execute_scripts` fires the
/// event, proving execution order.
#[test]
fn minimal_repro_preload_fills_version_spans() {
    let host = ElectronHost::new("minimal-repro", "1.0.0");
    let mut doc = ScriptDocument::from_html(
        "<html><body>Node.js <span id=\"node-version\"></span>, Chromium <span id=\"chrome-version\"></span>, Electron <span id=\"electron-version\"></span>.</body></html>",
        DocumentConfig::default(),
    )
    .without_timer_thread()
    .with_virtual_time();
    doc.install_electron_renderer(&host);
    assert!(
        doc.take_js_errors().is_empty(),
        "renderer bootstrap must install cleanly"
    );
    doc.eval(MINIMAL_REPRO_PRELOAD_JS);
    assert!(
        doc.take_js_errors().is_empty(),
        "verbatim preload.js must register without throwing"
    );
    doc.eval(
        "__strake_send_message('before:' + document.getElementById('node-version').innerText);",
    );
    doc.execute_scripts();
    assert!(
        doc.take_js_errors().is_empty(),
        "DOMContentLoaded dispatch must not throw"
    );
    doc.eval(
        "__strake_send_message('node:' + document.getElementById('node-version').innerText); \
         __strake_send_message('chrome:' + document.getElementById('chrome-version').innerText); \
         __strake_send_message('electron:' + document.getElementById('electron-version').innerText);",
    );
    assert!(doc.take_js_errors().is_empty());
    assert_eq!(
        doc.take_messages(),
        vec![
            "before:",
            "node:0.0.0-strake",
            "chrome:0.0.0-strake",
            "electron:0.0.0-strake",
        ]
    );
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

/// `process.env` inherits the host environ as a plain string map (Node
/// semantics): reads see host variables, feature flags pass through, and
/// writes stay on the snapshot.
#[test]
fn process_env_passes_through_host_environ() {
    // Read-only check (no env mutation: tests run in parallel threads, and
    // mutating the shared environ would race other tests' snapshots).
    let path = std::env::var("PATH").unwrap_or_default();
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "__strake_send_message('typeof:' + typeof process.env); \
         __strake_send_message('path:' + process.env.PATH); \
         __strake_send_message('has:' + ('PATH' in process.env)); \
         __strake_send_message('missing:' + process.env.STRAKE_TEST_ENV_DEFINITELY_ABSENT); \
         process.env.STRAKE_TEST_ENV_WRITE = 'written'; \
         __strake_send_message('write:' + process.env.STRAKE_TEST_ENV_WRITE);",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "process.env must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "typeof:object".to_string(),
            format!("path:{path}"),
            // Portable across OS case rules: on Windows the variable is
            // `Path` and the snapshot Proxy answers `PATH`; elsewhere the
            // exact-case key exists.
            "has:true".to_string(),
            "missing:undefined".to_string(),
            "write:written".to_string(),
        ]
    );
    assert!(
        std::env::var("STRAKE_TEST_ENV_WRITE").is_err(),
        "snapshot writes must not leak into the host environ"
    );
}

/// Node's `global` aliases the global object: `@electron/remote` reads
/// `const { Promise } = global` at load.
#[test]
fn node_global_alias_matches_global_this() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.eval(
        "__strake_send_message('same:' + (global === globalThis)); \
         __strake_send_message('process:' + (global.process === process));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "global alias must not throw, got {errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["same:true", "process:true"]);
}

#[test]
fn node_events_standin_covers_emitter_basics() {
    // Issue #16 canary slice: `require('node:events')` serves an
    // EventEmitter with Node's on/once/off/emit/listenerCount semantics.
    let (mut doc, _host) = main_doc();
    drop(doc.take_messages());
    doc.eval(
        "const { EventEmitter } = require('node:events'); \
         const { EventEmitter: Bare } = require('events'); \
         const em = new EventEmitter(); \
         __strake_send_message('ctor:' + (em instanceof EventEmitter) + '/' + (EventEmitter === Bare)); \
         let calls = []; \
         const a = (x) => calls.push('a' + x); \
         const b = (x) => calls.push('b' + x); \
         em.on('ev', a); \
         em.once('ev', b); \
         __strake_send_message('emit1:' + em.emit('ev', 1)); \
         __strake_send_message('emit2:' + em.emit('ev', 2)); \
         __strake_send_message('calls:' + calls.join(',')); \
         __strake_send_message('count:' + em.listenerCount('ev') + '/' + em.listeners('ev').length); \
         em.off('ev', a); \
         __strake_send_message('emit3:' + em.emit('ev', 3)); \
         em.on('other', a); \
         em.removeAllListeners('other'); \
         __strake_send_message('emit4:' + em.emit('other')); \
         em.on('x', a); \
         em.removeAllListeners(); \
         __strake_send_message('emit5:' + em.emit('x'));",
    );
    let js_errors = doc.take_js_errors();
    assert!(
        js_errors.is_empty(),
        "events stand-in must not throw, got {js_errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "ctor:true/true",
            "emit1:true",
            "emit2:true",
            "calls:a1,b1,a2",
            "count:1/1",
            "emit3:false",
            "emit4:false",
            "emit5:false",
        ]
    );
}

/// Loader helpers for issue #151: hermetic temp app dirs (no repo fixtures).
fn loader_temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("strake-loader-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp loader dir creates");
    dir
}

fn loader_doc_at(root: &std::path::Path) -> ScriptDocument {
    let host = ElectronHost::new("LoaderApp", "1.0.0");
    // The document clones the host's shared handle into its context, so
    // dropping `host` here keeps the state alive inside the document.
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.set_node_app_root(&root.to_string_lossy());
    doc
}

/// Relative-file requires resolve inside the app dir with extension and
/// index probing, plus JSON (issue #151).
#[test]
fn require_relative_resolves_with_probing_and_json() {
    let root = loader_temp_dir("relative");
    std::fs::create_dir_all(root.join("lib")).expect("lib dir");
    std::fs::write(
        root.join("lib/util.js"),
        "module.exports = { value: 42, from: __dirname };",
    )
    .expect("util.js writes");
    std::fs::write(root.join("lib/data.json"), r#"{"hello":"world"}"#).expect("data.json writes");
    std::fs::create_dir_all(root.join("lib/nested")).expect("nested dir");
    std::fs::write(
        root.join("lib/nested/index.js"),
        "module.exports = 'nested-index';",
    )
    .expect("index writes");

    let mut doc = loader_doc_at(&root);
    assert!(
        doc.take_js_errors().is_empty(),
        "loader bootstrap must install cleanly"
    );
    doc.eval(
        "const util = require('./lib/util'); \
         __strake_send_message('util:' + util.value); \
         const data = require('./lib/data.json'); \
         __strake_send_message('json:' + data.hello); \
         const nested = require('./lib/nested'); \
         __strake_send_message('nested:' + nested);",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "relative requires must not throw"
    );
    let messages = doc.take_messages();
    assert!(
        messages.iter().any(|message| message == "util:42"),
        "relative .js resolves, got {messages:?}"
    );
    assert!(
        messages.iter().any(|message| message == "json:world"),
        "relative .json resolves, got {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message == "nested:nested-index"),
        "directory index probing resolves, got {messages:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Bare specifiers resolve from the app's `node_modules` via `package.json`
/// `main`, and a missing package fails naming the package (issue #151).
#[test]
fn require_bare_resolves_node_modules_and_names_missing() {
    let root = loader_temp_dir("bare");
    let pkg_dir = root.join("node_modules/answer");
    std::fs::create_dir_all(&pkg_dir).expect("pkg dir");
    std::fs::write(
        pkg_dir.join("package.json"),
        r#"{"name":"answer","version":"1.0.0","main":"main.js"}"#,
    )
    .expect("package.json writes");
    std::fs::write(pkg_dir.join("main.js"), "module.exports = 42;").expect("main.js writes");

    let mut doc = loader_doc_at(&root);
    doc.eval(
        "const answer = require('answer'); \
         __strake_send_message('answer:' + answer);",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "bare package must resolve, got {:?}",
        doc.take_js_errors()
    );
    assert_eq!(doc.take_messages(), vec!["answer:42"]);

    doc.eval("require('no-such-pkg-xyz');");
    let errors = doc.take_js_errors();
    assert_eq!(
        errors.len(),
        1,
        "missing package throws once, got {errors:?}"
    );
    assert!(
        errors[0].contains("Cannot find module 'no-such-pkg-xyz'"),
        "missing error names the package, got {}",
        errors[0]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Core modules and native addons never mis-resolve via the file loader
/// (issue #151): `fs` resolves to the real core (issue #154), never to a
/// `node_modules` shadow, and `.node` files are refused.
#[test]
fn require_never_misresolves_core_or_native() {
    let root = loader_temp_dir("core");
    // A third-party `fs` in node_modules must NOT shadow the core: the
    // loader refuses cores before filesystem probing.
    let shadow = root.join("node_modules/fs");
    std::fs::create_dir_all(&shadow).expect("shadow dir");
    std::fs::write(shadow.join("index.js"), "module.exports = 'shadow';").expect("shadow writes");
    std::fs::write(root.join("evil.node"), "not a real addon").expect("node stub writes");

    let mut doc = loader_doc_at(&root);
    doc.eval(
        "const coreFs = require('fs'); \
         __strake_send_message('shadow:' + (coreFs === 'shadow') + '/' + (typeof coreFs.readFileSync));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "core fs resolves past the shadow, got {errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["shadow:false/function"]);
    doc.eval("require('./evil.node');");
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 1, ".node refuses, got {errors:?}");
    assert!(
        errors[0].contains("evil.node"),
        ".node error names the specifier, got {}",
        errors[0]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Required files evaluate in a per-file CommonJS function scope (Node
/// parity): top-level `const`/`let`/`class` must not collide across files
/// sharing the loader. Joplin boot hit this as `duplicate lexical
/// declaration` when `@electron/remote`'s `server.js` and its siblings each
/// declared TS-helper `const`s at top level.
#[test]
fn require_scopes_each_file_like_node() {
    let root = loader_temp_dir("scope");
    std::fs::write(
        root.join("alpha.js"),
        "const helper = 'alpha'; let counter = 1; class Box { v() { return helper + counter; } } \
         module.exports = { box: new Box(), top: this === module.exports };",
    )
    .expect("alpha writes");
    std::fs::write(
        root.join("beta.js"),
        "const helper = 'beta'; let counter = 41; class Box { v() { return helper + counter; } } \
         module.exports = { box: new Box(), top: this === module.exports };",
    )
    .expect("beta writes");
    let mut doc = loader_doc_at(&root);
    doc.eval(
        "const a = require('./alpha.js'); \
         const b = require('./beta.js'); \
         __strake_send_message('a:' + a.box.v()); \
         __strake_send_message('b:' + b.box.v()); \
         __strake_send_message('top:' + (a.top && b.top)); \
         __strake_send_message('leak:' + (typeof helper === 'undefined' && typeof Box === 'undefined'));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "same-named top-level bindings must not collide, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["a:alpha1", "b:beta41", "top:true", "leak:true"]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Boa 0.22 rejects bare `of`/`let` arrow params (valid per spec, emitted by
/// real bundlers — Joplin's `main.bundle.js`): eval must transparently retry
/// a parenthesized copy and run it, reporting no errors.
#[test]
fn keyword_arrow_params_evaluate_via_fallback() {
    let host = ElectronHost::new("KeywordArrow", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    assert!(
        doc.take_js_errors().is_empty(),
        "electron bootstrap must install cleanly"
    );
    doc.eval(
        "function y(f) { return f; } \
         var zh = y(of => of + 1); \
         var lh = y(let => let * 2); \
         __strake_send_message('kw:' + zh(41) + '/' + lh(21));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "keyword arrow params must evaluate without throwing, got {errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["kw:42/42"]);
}

/// Issue #154 helpers: hermetic temp dir with a canonical path (macOS
/// symlinks `/var` to `/private/var`; an uncanonicalized temp dir would
/// escape a grant scope built from its own spelling).
fn fs_temp_dir(name: &str) -> std::path::PathBuf {
    let dir = loader_temp_dir(name);
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// Forward-slash spelling of a path for embedding in JS source (Windows
/// accepts `/` separators; a raw `\` would parse as a JS escape).
fn js_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A main-process document whose host grants fs read+write under `root`
/// (issue #154): the embedder-approved capability manifest in action.
fn fs_doc_at(root: &std::path::Path) -> ScriptDocument {
    use strake_electron_compat::{PathScope, PermissionManifest};
    let scope = format!("{}/*", js_path(root));
    fs_doc_with(
        PermissionManifest {
            fs_read: vec![PathScope::new(&scope)],
            fs_write: vec![PathScope::new(&scope)],
            ..Default::default()
        },
        root,
    )
}

/// A main-process document with an explicit permission manifest.
fn fs_doc_with(
    manifest: strake_electron_compat::PermissionManifest,
    root: &std::path::Path,
) -> ScriptDocument {
    let host = ElectronHost::new("FsApp", "1.0.0").with_permissions(manifest);
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    assert!(
        doc.take_js_errors().is_empty(),
        "electron bootstrap must install cleanly"
    );
    doc.set_node_app_root(&js_path(root));
    doc
}

/// Issue #154: with the default deny-by-default host, `require('fs')`
/// resolves but every operation is refused: reads/writes throw `EACCES`,
/// and `existsSync` reports `false` even for files that exist (no oracle).
#[test]
fn node_fs_denied_without_grant() {
    let root = fs_temp_dir("denied");
    std::fs::write(root.join("secret.txt"), "TOP SECRET").expect("secret writes");
    let probe = std::env::temp_dir().join("strake-denied-probe.txt");
    let _ = std::fs::remove_file(&probe);
    let mut doc = loader_doc_at(&root);
    let script = "const fs = require('fs'); \
         __strake_send_message('typeof:' + typeof fs.readFileSync); \
         let readCode = 'none'; \
         try { fs.readFileSync('SECRET_PLACEHOLDER'); } catch (e) { readCode = e.code; } \
         __strake_send_message('read:' + readCode); \
         let writeCode = 'none'; \
         try { fs.writeFileSync('PROBE_PLACEHOLDER', 'x'); } catch (e) { writeCode = e.code; } \
         __strake_send_message('write:' + writeCode); \
         __strake_send_message('exists:' + fs.existsSync('SECRET_PLACEHOLDER'));"
        .replace("SECRET_PLACEHOLDER", &js_path(&root.join("secret.txt")))
        .replace("PROBE_PLACEHOLDER", &js_path(&probe));
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "denied fs calls must throw catchable errors, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "typeof:function",
            "read:EACCES",
            "write:EACCES",
            "exists:false",
        ]
    );
    assert!(
        !probe.exists(),
        "a denied write must not touch the host filesystem"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #154: with a grant covering the app dir, the sync subset is real
/// (exists/access, recursive mkdir, read/write, stat, readdir,
/// unlink/rename) with Node error codes (`ENOENT`, …).
#[test]
fn node_fs_sync_subset_with_grant() {
    let root = fs_temp_dir("subset");
    let mut doc = fs_doc_at(&root);
    let script = "const fs = require('fs'); \
         const dir = DIR_PLACEHOLDER; \
         fs.mkdirSync(dir + '/a/b/c', { recursive: true }); \
         fs.writeFileSync(dir + '/hello.txt', 'hello strake'); \
         __strake_send_message('read:' + fs.readFileSync(dir + '/hello.txt', 'utf8')); \
         const raw = fs.readFileSync(dir + '/hello.txt'); \
         __strake_send_message('buffer:' + Buffer.isBuffer(raw) + '/' + raw.length); \
         fs.appendFileSync(dir + '/hello.txt', '!'); \
         __strake_send_message('appended:' + fs.readFileSync(dir + '/hello.txt', 'utf8')); \
         const st = fs.statSync(dir + '/hello.txt'); \
         __strake_send_message('stat:' + st.isFile() + '/' + st.isDirectory() + '/' + st.size + '/' + (st.mtimeMs > 0) + '/' + (st.mtime instanceof Date)); \
         __strake_send_message('dir:' + fs.statSync(dir + '/a').isDirectory()); \
         __strake_send_message('ls:' + fs.readdirSync(dir).sort().join(',')); \
         __strake_send_message('exists:' + fs.existsSync(dir + '/hello.txt') + '/' + fs.existsSync(dir + '/missing.txt')); \
         fs.accessSync(dir + '/hello.txt'); \
         __strake_send_message('access:ok'); \
         let accessCode = 'none'; \
         try { fs.accessSync(dir + '/missing.txt'); } catch (e) { accessCode = e.code; } \
         __strake_send_message('access-missing:' + accessCode); \
         let readCode = 'none'; \
         try { fs.readFileSync(dir + '/missing.txt'); } catch (e) { readCode = e.code; } \
         __strake_send_message('read-missing:' + readCode); \
         fs.copyFileSync(dir + '/hello.txt', dir + '/copy.txt'); \
         fs.renameSync(dir + '/copy.txt', dir + '/moved.txt'); \
         __strake_send_message('moved:' + fs.readFileSync(dir + '/moved.txt', 'utf8')); \
         fs.unlinkSync(dir + '/moved.txt'); \
         __strake_send_message('unlinked:' + fs.existsSync(dir + '/moved.txt')); \
         __strake_send_message('realpath:' + String(fs.realpathSync(dir + '/hello.txt')).endsWith('hello.txt'));"
        .replace("DIR_PLACEHOLDER", &format!("'{}'", js_path(&root)));
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "granted fs calls must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "read:hello strake",
            "buffer:true/12",
            "appended:hello strake!",
            "stat:true/false/13/true/true",
            "dir:true",
            "ls:a,hello.txt",
            "exists:true/false",
            "access:ok",
            "access-missing:ENOENT",
            "read-missing:ENOENT",
            "moved:hello strake!",
            "unlinked:false",
            "realpath:true",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #155: `fs/promises` — Joplin (`graceful-fs`, lock heartbeats, shim
/// installs) awaits `writeFile`/`readFile`/`stat`/`utimes`/`realpath`/
/// `readlink`/`readdir`/`lstat`. Same sync I/O underneath, but completions
/// always land on microtasks (no sync zalgo); `require('fs').promises` is
/// the same object.
#[test]
fn node_fs_promises_roundtrip() {
    let root = fs_temp_dir("fspromises");
    let mut doc = fs_doc_at(&root);
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", root.join("link.txt")).ok();
    let script = "const fsp = require('fs/promises'); \
         __strake_send_message('prefix:' + (require('node:fs/promises') === fsp) + '/' + (require('fs').promises === fsp)); \
         let sync = true; \
         __strake_send_message('sync:' + sync); \
         const dir = DIR_PLACEHOLDER; \
         const file = dir + '/a.txt'; \
         fsp.writeFile(file, 'hello promises', 'utf8')
           .then(() => fsp.readFile(file, 'utf8'))
           .then((text) => __strake_send_message('write-read:' + text))
           .then(() => fsp.stat(file))
           .then((st) => __strake_send_message('stat:' + st.size + '/' + st.isFile()))
           .then(() => fsp.mkdir(dir + '/sub'))
           .then(() => fsp.readdir(dir))
           .then((entries) => __strake_send_message('readdir:' + entries.sort().join(',')))
           .then(() => fsp.utimes(file, 1700000000, 1700000000))
           .then(() => fsp.stat(file))
           .then((st) => __strake_send_message('utimes:' + (Math.abs(st.mtimeMs - 1700000000000) < 1000)))
           .then(() => fsp.realpath(file))
           .then((p) => __strake_send_message('realpath:' + String(p).endsWith('a.txt')))
           .then(() => fsp.lstat(dir))
           .then((st) => __strake_send_message('lstat:' + st.isDirectory()))
           .then(() => fsp.readlink(dir + '/link.txt').then((t) => 'link:' + t, () => 'link:skip'))
           .then((m) => __strake_send_message(m))
           .catch((e) => __strake_send_message('failed:' + (e && e.code))); \
         sync = false;"
        .replace("DIR_PLACEHOLDER", &format!("'{}'", js_path(&root)));
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "promises must not throw, got {errors:?}");
    // No `symlinkSync` stand-in: the readlink fixture only exists on unix.
    #[cfg(unix)]
    let (ls, link) = ("readdir:a.txt,link.txt,sub", "link:a.txt");
    #[cfg(not(unix))]
    let (ls, link) = ("readdir:a.txt,sub", "link:skip");
    assert_eq!(
        doc.take_messages(),
        vec![
            "prefix:true/true",
            "sync:true",
            "write-read:hello promises",
            "stat:14/true",
            ls,
            "utimes:true",
            "realpath:true",
            "lstat:true",
            link,
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #154: lexical `..` escapes cannot walk out of the grant, even when
/// the target exists — the denial names `EACCES`, never the file content.
#[test]
fn node_fs_escape_outside_grant_is_denied() {
    let root = fs_temp_dir("escape");
    let inner = root.join("inner");
    std::fs::create_dir_all(&inner).expect("inner dir creates");
    std::fs::write(root.join("outside.txt"), "TOP SECRET").expect("outside writes");
    use strake_electron_compat::{PathScope, PermissionManifest};
    let scope = format!("{}/*", js_path(&inner));
    let mut doc = fs_doc_with(
        PermissionManifest {
            fs_read: vec![PathScope::new(&scope)],
            fs_write: vec![PathScope::new(&scope)],
            ..Default::default()
        },
        &inner,
    );
    let script = "const fs = require('fs'); \
         let code = 'none'; \
         let body = ''; \
         try { body = String(fs.readFileSync(EVIL_PLACEHOLDER)); } catch (e) { code = e.code; } \
         __strake_send_message('escape:' + code + '/' + body);"
        .replace(
            "EVIL_PLACEHOLDER",
            &format!("'{}'", js_path(&inner.join("..").join("outside.txt"))),
        );
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "escaped reads must throw catchable errors, got {errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["escape:EACCES/"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #154: `Buffer` minimum (alloc, from string/bytes, toString,
/// length, slice), shared between `require('buffer')` and the global.
#[test]
fn node_buffer_minimum() {
    let root = fs_temp_dir("buffer");
    let mut doc = fs_doc_at(&root);
    doc.eval(
        "const { Buffer: Required } = require('buffer'); \
         __strake_send_message('same:' + (Required === globalThis.Buffer)); \
         __strake_send_message('zero:' + Buffer.alloc(4).toString('hex')); \
         __strake_send_message('fill:' + Buffer.alloc(3, 65).toString()); \
         const text = Buffer.from('h\u{00e9}llo'); \
         __strake_send_message('utf8:' + text.length + '/' + text.toString()); \
         __strake_send_message('hex:' + Buffer.from('deadbeef', 'hex').toString('base64')); \
         __strake_send_message('bytes:' + Buffer.from([104, 105]).toString()); \
         __strake_send_message('slice:' + text.slice(1, 3).length + '/' + Buffer.isBuffer(text.slice(0, 1))); \
         __strake_send_message('u8:' + (text instanceof Uint8Array));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "buffer ops must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "same:true",
            "zero:00000000",
            "fill:AAA",
            "utf8:6/h\u{00e9}llo",
            "hex:3q2+7w==",
            "bytes:hi",
            "slice:2/true",
            "u8:true",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #155: `Buffer.allocUnsafe` — Joplin's `uuid` parses namespace UUIDs
/// into `Buffer.allocUnsafe(16)`. Headless memory is always fresh, so the
/// result is zero-filled (strictly safer than Node's pooled garbage, and
/// identical for callers that fill the buffer before reading).
#[test]
fn node_buffer_alloc_unsafe() {
    let root = fs_temp_dir("allocunsafe");
    let mut doc = fs_doc_at(&root);
    doc.eval(
        "const buf = Buffer.allocUnsafe(16); \
         __strake_send_message('len:' + buf.length + '/' + Buffer.isBuffer(buf)); \
         __strake_send_message('zero:' + buf.toString('hex')); \
         buf[0] = 255; \
         __strake_send_message('write:' + buf[0]); \
         let bad = 'none'; \
         try { Buffer.allocUnsafe(-1); } catch (e) { bad = e.constructor.name; } \
         __strake_send_message('bad:' + bad);",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "allocUnsafe must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "len:16/true",
            "zero:00000000000000000000000000000000",
            "write:255",
            "bad:RangeError",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #154: `stream` `.Stream` base (an `EventEmitter` subclass),
/// `util` (`format` subset, `inherits`, `debuglog`), and `constants`.
#[test]
fn node_stream_util_constants() {
    let root = fs_temp_dir("streamutil");
    let mut doc = fs_doc_at(&root);
    doc.eval(
        "const { Stream } = require('stream'); \
         const { EventEmitter } = require('events'); \
         class S extends Stream {} \
         const s = new S(); \
         __strake_send_message('emitter:' + (s instanceof EventEmitter)); \
         s.on('x', (v) => __strake_send_message('emit:' + v)); \
         s.emit('x', 7); \
         const util = require('util'); \
         __strake_send_message('format:' + util.format('%s=%d %j %%', 'a', 1, { b: 2 })); \
         function Base() {} \
         function Child() { Base.call(this); } \
         util.inherits(Child, Base); \
         __strake_send_message('inherits:' + (new Child() instanceof Base)); \
         __strake_send_message('debuglog:' + (typeof util.debuglog('test'))); \
         const c = require('constants'); \
         const fc = require('fs').constants; \
         __strake_send_message('stable:' + [c.O_RDONLY, c.O_WRONLY, c.O_RDWR, c.F_OK, c.R_OK, c.W_OK, c.X_OK, c.COPYFILE_EXCL].join(',')); \
         __strake_send_message('par:' + (fc.O_RDONLY === c.O_RDONLY && fc.F_OK === c.F_OK && typeof c.O_CREAT === 'number'));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "stream/util/constants must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "emitter:true",
            "emit:7",
            "format:a=1 {\"b\":2} %",
            "inherits:true",
            "debuglog:function",
            "stable:0,1,2,0,4,2,1,1",
            "par:true",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issues #154/#155: the sync fd layer (`openSync`/`readSync`/`writeSync`/
/// `closeSync`) works with string and numeric flags, refuses unknown fds
/// with `EBADF`, and refuses `O_EXCL` creation over an existing file.
#[test]
fn node_fs_fd_sync_layer() {
    let root = fs_temp_dir("fdlay");
    let mut doc = fs_doc_at(&root);
    let script = "const fs = require('fs'); \
         const dir = DIR_PLACEHOLDER; \
         const fd = fs.openSync(dir + '/fd.txt', 'w'); \
         __strake_send_message('fd:' + (typeof fd) + '/' + (fd >= 0)); \
         const buf = Buffer.from('hello fd'); \
         __strake_send_message('written:' + fs.writeSync(fd, buf, 0, buf.length, null)); \
         fs.closeSync(fd); \
         const rfd = fs.openSync(dir + '/fd.txt', 'r'); \
         const out = Buffer.alloc(16); \
         const n = fs.readSync(rfd, out, 0, 16, 0); \
         __strake_send_message('read:' + n + '/' + out.slice(0, n).toString()); \
         fs.closeSync(rfd); \
         const cfd = fs.openSync(dir + '/fd2.txt', fs.constants.O_CREAT | fs.constants.O_WRONLY); \
         fs.closeSync(cfd); \
         __strake_send_message('created:' + fs.existsSync(dir + '/fd2.txt')); \
         let excl = 'none'; \
         try { fs.openSync(dir + '/fd2.txt', fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_WRONLY); } catch (e) { excl = e.code; } \
         __strake_send_message('excl:' + excl); \
         let bad = 'none'; \
         try { fs.readSync(9999, Buffer.alloc(4), 0, 4, 0); } catch (e) { bad = e.code; } \
         __strake_send_message('bad:' + bad); \
         let badClose = 'none'; \
         try { fs.closeSync(9999); } catch (e) { badClose = e.code; } \
         __strake_send_message('badClose:' + badClose);"
        .replace("DIR_PLACEHOLDER", &format!("'{}'", js_path(&root)));
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "fd ops must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "fd:number/true",
            "written:8",
            "read:8/hello fd",
            "created:true",
            "excl:EEXIST",
            "bad:EBADF",
            "badClose:EBADF",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issues #154/#155: the callback `fs` variants complete with Node `(err,
/// result)` shapes, so `graceful-fs` wrappers can delegate to them.
/// Completion is deferred through the microtask queue (never synchronous —
/// real wrappers attach listeners after the call and assume the callback
/// runs later; true Tokio-backed async is issue #141), so independent chains
/// interleave FIFO: `open` before `mkdir`, then each chain's nested call in
/// turn.
#[test]
fn node_fs_async_variants_complete() {
    let root = fs_temp_dir("fdasy");
    let mut doc = fs_doc_at(&root);
    let script = "const fs = require('fs'); \
         const dir = DIR_PLACEHOLDER; \
         fs.open(dir + '/a.txt', 'w', (err, fd) => { \
           __strake_send_message('open:' + (err ? err.code : 'ok/' + (typeof fd))); \
           const buf = Buffer.from('async-writes'); \
           fs.write(fd, buf, 0, buf.length, null, (err2, wrote) => { \
             __strake_send_message('write:' + (err2 ? err2.code : wrote)); \
             fs.close(fd, (err3) => { \
               __strake_send_message('close:' + (err3 ? err3.code : 'ok')); \
               fs.readFile(dir + '/a.txt', 'utf8', (err4, text) => { \
                 __strake_send_message('read:' + (err4 ? err4.code : text)); \
               }); \
             }); \
           }); \
         }); \
         fs.mkdir(dir + '/sub', (err5) => { \
           __strake_send_message('mkdir:' + (err5 ? err5.code : 'ok')); \
           fs.readdir(dir, (err6, files) => { \
             __strake_send_message('ls:' + (err6 ? err6.code : files.sort().join(','))); \
           }); \
         });"
    .replace("DIR_PLACEHOLDER", &format!("'{}'", js_path(&root)));
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "async fs must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "open:ok/number",
            "mkdir:ok",
            "write:12",
            "ls:a.txt,sub",
            "close:ok",
            "read:async-writes",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issues #154/#155: `graceful-fs` wraps `ReadStream` in a plain function
/// that delegates via `.apply` and swaps the prototype's `open` — which
/// throws on an ES class constructor. Our streams stay plain functions so
/// the exact `graceful-fs` shape works and data still flows.
#[test]
fn node_fs_streams_survive_graceful_wrapper() {
    let root = fs_temp_dir("gwrap");
    let mut doc = fs_doc_at(&root);
    let script = "const fs = require('fs'); \
         const dir = DIR_PLACEHOLDER; \
         fs.writeFileSync(dir + '/g.txt', 'graceful-interop'); \
         function GReadStream(path, options) { \
           if (this instanceof GReadStream) return fs.ReadStream.apply(this, arguments); \
           return new GReadStream(path, options); \
         } \
         GReadStream.prototype = Object.create(fs.ReadStream.prototype); \
         let customOpened = false; \
         GReadStream.prototype.open = function () { \
           customOpened = true; \
           fs.ReadStream.prototype.open.call(this); \
         }; \
         const rs = new GReadStream(dir + '/g.txt'); \
         const seen = []; \
         rs.on('data', (c) => seen.push(String(c))); \
         rs.on('end', () => __strake_send_message('g:' + seen.join('') + '/' + customOpened)); \
         const rs2 = new GReadStream(dir + '/g.txt'); \
         const seen2 = []; \
         rs2.on('data', (c) => seen2.push(String(c))); \
         rs2.on('end', () => __strake_send_message('g2:' + seen2.join(''))); \
         rs2.open(); \
         rs2.read();"
        .replace("DIR_PLACEHOLDER", &format!("'{}'", js_path(&root)));
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "wrapped streams must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["g:graceful-interop/true", "g2:graceful-interop",]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `process.nextTick` runs callbacks (FIFO microtask approximation —
/// documented on the bootstrap; true priority ordering is out of scope).
#[test]
fn node_process_next_tick_runs_callbacks() {
    let mut doc = loader_doc_at(&fs_temp_dir("tick"));
    doc.eval(
        "const order = []; \
         order.push('sync'); \
         process.nextTick(() => order.push('tick')); \
         Promise.resolve().then(() => order.push('micro')); \
         queueMicrotask(() => __strake_send_message('order:' + order.join(',')));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "nextTick must not throw, got {errors:?}");
    assert_eq!(doc.take_messages(), vec!["order:sync,tick,micro"]);
}

/// Issue #154: file streams work end to end — `createReadStream` emits
/// `data`/`end`, `createWriteStream` collects `write` chunks and commits on
/// `end` with `finish`, and `pipe` connects them. The read pump runs on the
/// microtask queue, which `eval` drains before returning, so message order
/// is deterministic.
#[test]
fn node_fs_streams_roundtrip() {
    let root = fs_temp_dir("streams");
    let mut doc = fs_doc_at(&root);
    let script = "const fs = require('fs'); \
         const dir = DIR_PLACEHOLDER; \
         fs.writeFileSync(dir + '/in.txt', 'stream-me'); \
         __strake_send_message('classes:' + (typeof fs.ReadStream) + '/' + (typeof fs.createReadStream) + '/' + (typeof fs.createWriteStream)); \
         const rs = fs.createReadStream(dir + '/in.txt'); \
         const seen = []; \
         rs.on('data', (c) => seen.push(String(c))); \
         rs.on('end', () => __strake_send_message('stream:' + seen.join(''))); \
         const ws = fs.createWriteStream(dir + '/out.txt'); \
         ws.on('finish', () => __strake_send_message('written:' + fs.readFileSync(dir + '/out.txt', 'utf8'))); \
         ws.write('a'); \
         ws.write(Buffer.from('b')); \
         ws.end('c'); \
         const piped = fs.createWriteStream(dir + '/piped.txt'); \
         piped.on('finish', () => __strake_send_message('piped:' + fs.readFileSync(dir + '/piped.txt', 'utf8'))); \
         fs.createReadStream(dir + '/in.txt').pipe(piped);"
        .replace("DIR_PLACEHOLDER", &format!("'{}'", js_path(&root)));
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "stream ops must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "classes:function/function/function",
            "written:abc",
            "stream:stream-me",
            "piped:stream-me",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Version-sniffing loaders (`graceful-fs` et al.) read `process.version`:
/// it must exist and carry a `v`-prefixed Node-shaped version.
#[test]
fn node_process_version_present() {
    let mut doc = loader_doc_at(&fs_temp_dir("version"));
    doc.eval(
        "__strake_send_message('version:' + (typeof process.version) + ':' + String(process.version).startsWith('v'));",
    );
    assert!(doc.take_js_errors().is_empty());
    assert_eq!(doc.take_messages(), vec!["version:string:true"]);
}

/// Issue #155: `require`d files get the same keyword-arrow repair as eval'd
/// sources — real bundlers emit bare `of =>` chunk params, and split
/// bundles load as modules, not as the main entry.
#[test]
fn node_require_repairs_keyword_arrow_params() {
    let root = fs_temp_dir("arrowreq");
    std::fs::write(root.join("kw.js"), "module.exports = (of => of * 2)(21);")
        .expect("arrow fixture writes");
    let mut doc = fs_doc_at(&root);
    doc.eval(
        "const kw = require('./kw.js'); \
         __strake_send_message('kw:' + kw);",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "arrow require must not throw, got {errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["kw:42"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #155: `process.emitWarning` exists and warns without throwing —
/// real shims (`fs-extra`) call it when `fs.realpath.native` is absent.
#[test]
fn node_process_emit_warning_warns_without_throwing() {
    let mut doc = loader_doc_at(&fs_temp_dir("emitwarn"));
    doc.eval(
        "__strake_send_message('typeof:' + (typeof process.emitWarning)); \
         process.emitWarning('fs.realpath.native is not a function', 'Warning', 'FS_EXTRA_WARN'); \
         __strake_send_message('survived:true');",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "emitWarning must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["typeof:function", "survived:true"]
    );
}

/// Issue #155: `node:assert` core module — real bundles require it at
/// import time. Callable assertion plus the comparison subset callers use,
/// with Node's `ERR_ASSERTION` shape on failure.
#[test]
fn node_assert_subset() {
    let mut doc = loader_doc_at(&fs_temp_dir("assert"));
    doc.eval(
        "const assert = require('assert'); \
         assert(true); \
         assert.ok('x'); \
         assert.equal(1, '1'); \
         assert.strictEqual(1, 1); \
         assert.deepStrictEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] }); \
         let code = 'none'; \
         try { assert.strictEqual(1, 2); } catch (e) { code = e.code + '/' + e.operator; } \
         __strake_send_message('assert:' + code); \
         __strake_send_message('same:' + (require('node:assert') === assert));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "assert must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec!["assert:ERR_ASSERTION/strictEqual", "same:true"]
    );
}

/// Issue #155: `node:child_process` resolves (real updater/sentry chunks
/// bind it at import time) but spawning throws a coded error — process
/// spawning is capability-gated follow-up work, never silent fakery.
#[test]
fn node_child_process_resolves_but_spawning_is_coded() {
    let mut doc = loader_doc_at(&fs_temp_dir("childproc"));
    doc.eval(
        "const cp = require('child_process'); \
         __strake_send_message('names:' + ['exec', 'execFile', 'spawn', 'execSync', 'spawnSync'].map((k) => typeof cp[k]).join(',')); \
         let code = 'none'; \
         try { cp.execSync('x'); } catch (e) { code = e.code; } \
         __strake_send_message('execSync:' + code); \
         __strake_send_message('same:' + (require('node:child_process') === cp));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "child_process surface must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "names:function,function,function,function,function",
            "execSync:ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
            "same:true",
        ]
    );
}

/// Issue #155: `node:crypto` hashing subset — real digests verified against
/// published vectors (md5/sha1/sha256, HMAC, PBKDF2). Entropy is real OS
/// randomness (`getrandom`, user-approved); only ciphers stay
/// coded-unavailable: no fake crypto.
#[test]
fn node_crypto_hash_subset_with_vectors() {
    let mut doc = loader_doc_at(&fs_temp_dir("crypto"));
    doc.eval(
        "const crypto = require('crypto'); \
         __strake_send_message('md5:' + crypto.createHash('md5').update('abc').digest('hex')); \
         __strake_send_message('sha1:' + crypto.createHash('sha1').update('abc').digest('hex')); \
         __strake_send_message('sha256:' + crypto.createHash('sha256').update('a').update('bc').digest('hex')); \
         __strake_send_message('sha256e:' + crypto.createHash('sha256').update('').digest('hex')); \
         __strake_send_message('hmac:' + crypto.createHmac('sha256', 'key').update('The quick brown fox jumps over the lazy dog').digest('hex')); \
         __strake_send_message('pbkdf2:' + crypto.pbkdf2Sync('password', 'salt', 1, 32, 'sha256').toString('hex')); \
         let randomCode = 'none'; \
         try { randomCode = crypto.randomBytes(4).length === 4 ? 'ok' : 'short'; } catch (e) { randomCode = e.code; } \
         let cipherCode = 'none'; \
         try { crypto.createCipheriv('aes-256-cbc', Buffer.alloc(32), Buffer.alloc(16)); } catch (e) { cipherCode = e.code; } \
         __strake_send_message('gated:' + randomCode + '/' + cipherCode);",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "crypto must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "md5:900150983cd24fb0d6963f7d28e17f72",
            "sha1:a9993e364706816aba3e25717850c26c9cd0d89d",
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "sha256e:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "hmac:f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8",
            "pbkdf2:120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b",
            "gated:ok/ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ]
    );
}

/// Issue #155: `node:crypto` entropy — Joplin's real bundle seeds `uuid` via
/// `crypto.randomBytes(16)` at import time, so boot needs real randomness
/// (sync + callback forms) and v4 `randomUUID`s. Ciphers stay gated.
#[test]
fn node_crypto_random_bytes_and_uuid() {
    let mut doc = loader_doc_at(&fs_temp_dir("entropy"));
    doc.eval(
        "const crypto = require('crypto'); \
         const a = crypto.randomBytes(16); \
         const b = crypto.randomBytes(16); \
         __strake_send_message('sync:' + a.length + '/' + b.length + '/' + (a.toString('hex') === b.toString('hex') ? 'same' : 'uniq')); \
         __strake_send_message('empty:' + crypto.randomBytes(0).length); \
         crypto.randomBytes(8, (err, buf) => { \
           __strake_send_message('async:' + (err ? err.code : buf.length)); \
         }); \
         const u1 = crypto.randomUUID(); \
         const u2 = crypto.randomUUID(); \
         const v4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/; \
         __strake_send_message('uuid:' + v4.test(u1) + '/' + v4.test(u2) + '/' + (u1 === u2 ? 'same' : 'uniq'));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "entropy must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "sync:16/16/uniq",
            "empty:0",
            "uuid:true/true/uniq",
            "async:8"
        ]
    );
}

/// Issue #155: `node:os` surface — Joplin's updater, Sentry context, and
/// `human-signals` read `platform`/`arch`/`release`/`hostname`/`homedir`/
/// `tmpdir`/mem/`cpus` and `constants.signals` at import time. Values are
/// real host facts where `std` can see them (Linux `/proc`, env, `temp_dir`,
/// parallelism); unknown numerics stay `0` and unknown strings stay marked.
#[test]
fn node_os_host_facts() {
    let mut doc = loader_doc_at(&fs_temp_dir("osfacts"));
    doc.eval(
        "const os = require('os'); \
         __strake_send_message('plat:' + (typeof os.platform() === 'string' && typeof os.arch() === 'string')); \
         __strake_send_message('strs:' + [os.release(), os.hostname(), os.homedir(), os.tmpdir()].map((s) => typeof s === 'string' && s.length > 0).join(',')); \
         __strake_send_message('eol:' + (os.EOL === '\\n' || os.EOL === '\\r\\n')); \
         __strake_send_message('nums:' + [os.uptime(), os.totalmem(), os.freemem()].map((n) => typeof n === 'number' && n >= 0).join(',')); \
         const cpus = os.cpus(); \
         __strake_send_message('cpus:' + (Array.isArray(cpus) && cpus.length >= 1 && typeof cpus[0].model === 'string' && typeof cpus[0].times === 'object')); \
         __strake_send_message('sig:' + os.constants.signals.SIGINT + '/' + os.constants.signals.SIGTERM + '/' + (os.constants.signals.SIGWINCH !== undefined)); \
         __strake_send_message('node:' + (require('node:os') === os));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "os must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "plat:true",
            "strs:true,true,true,true",
            "eol:true",
            "nums:true,true,true",
            "cpus:true",
            "sig:2/15/true",
            "node:true",
        ]
    );
}

/// Issue #155: `node:zlib` — Joplin's AppUpdater needs `gzipSync`/
/// `gunzipSync`, and its transports use `createGzip`/`createGunzip`/
/// `createInflate` plus `inflateRawSync`. Fixed blobs are Python-`zlib`
/// oracles (independent of our decoder); round-trips prove the encoder.
#[test]
fn node_zlib_sync_and_streams() {
    let mut doc = loader_doc_at(&fs_temp_dir("zlibrt"));
    doc.eval(
        "const zlib = require('zlib'); \
         const gzipBlob = [31,139,8,0,0,0,0,0,2,255,203,72,205,201,201,87,40,46,41,74,204,78,5,0,253,44,228,113,12,0,0,0]; \
         const rawBlob = [203,72,205,201,201,87,40,46,41,74,204,78,5,0]; \
         const wrapBlob = [120,156,203,72,205,201,201,87,40,46,41,74,204,78,5,0,30,187,4,191]; \
         __strake_send_message('gunzip:' + zlib.gunzipSync(Buffer.from(gzipBlob)).toString()); \
         __strake_send_message('raw:' + zlib.inflateRawSync(Buffer.from(rawBlob)).toString()); \
         __strake_send_message('roundtrip:' + zlib.gunzipSync(zlib.gzipSync('hello roundtrip')).toString()); \
         __strake_send_message('flush:' + zlib.Z_SYNC_FLUSH); \
         const flat = (parts) => { const out = []; for (const p of parts) { const v = new Uint8Array(p.buffer, p.byteOffset, p.byteLength); for (let i = 0; i < v.length; i++) out.push(v[i]); } return Buffer.from(out); }; \
         const gz = zlib.createGzip(); \
         const gzParts = []; \
         gz.on('data', (c) => gzParts.push(c)); \
         gz.on('end', () => __strake_send_message('stream-gzip:' + zlib.gunzipSync(flat(gzParts)).toString())); \
         gz.write('hello '); \
         gz.end('strake'); \
         const un = zlib.createGunzip(); \
         const unParts = []; \
         un.on('data', (c) => unParts.push(c)); \
         un.on('end', () => __strake_send_message('stream-gunzip:' + flat(unParts).toString())); \
         un.end(Buffer.from(gzipBlob)); \
         const inf = zlib.createInflate(); \
         const infParts = []; \
         inf.on('data', (c) => infParts.push(c)); \
         inf.on('end', () => __strake_send_message('stream-inflate:' + flat(infParts).toString())); \
         inf.end(Buffer.from(wrapBlob));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "zlib must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "gunzip:hello strake",
            "raw:hello strake",
            "roundtrip:hello roundtrip",
            "flush:2",
            "stream-gzip:hello strake",
            "stream-gunzip:hello strake",
            "stream-inflate:hello strake",
        ]
    );
}

/// Issue #155: `http`/`https` client — Joplin's updater, `got`, Sentry, and
/// `form-data` transports call `http(s).request` with `Agent`s. The client
/// performs real transfers (blocking `reqwest` under the hood, responses on
/// microtasks); `createServer().listen()` stays coded-unavailable (the
/// threaded serving bridge is follow-up work — Joplin's clipper autostart
/// defaults off, so boot never listens).
#[test]
fn node_http_request_roundtrip() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    // Canned HTTP/1.1 fixture (test-only, up to 3 connections): records the
    // raw request, then answers 201 with an echo header.
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("http fixture binds localhost");
    let port = listener.local_addr().expect("http fixture port").port();
    listener
        .set_nonblocking(true)
        .expect("http fixture nonblocking");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let seen_server = seen.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut served = 0;
        while Instant::now() < deadline && served < 3 {
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                if let Some(head_end) = buf
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|pos| pos + 4)
                {
                    let head = String::from_utf8_lossy(&buf[..head_end]);
                    let want: usize = head
                        .lines()
                        .filter_map(|line| {
                            line.strip_prefix("Content-Length:")
                                .or_else(|| line.strip_prefix("content-length:"))
                        })
                        .filter_map(|value| value.trim().parse().ok())
                        .next()
                        .unwrap_or(0);
                    if buf.len() >= head_end + want {
                        break;
                    }
                }
                if buf.len() > 65536 {
                    break;
                }
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            // The POST arrives first; keep it for the assertions below.
            let mut guard = seen_server.lock().expect("http fixture lock");
            if guard.is_empty() {
                *guard = buf;
            }
            let response = b"HTTP/1.1 201 Created\r\nContent-Length: 4\r\nX-Echo: got-it\r\nConnection: close\r\n\r\npong";
            let _ = stream.write_all(response);
            served += 1;
        }
    });
    let mut doc = loader_doc_at(&fs_temp_dir("httprt"));
    let eval_js = "const http = require('http'); \
         __strake_send_message('agent:' + (http.globalAgent instanceof http.Agent) + '/' + (typeof http.Agent.prototype.addRequest === 'function')); \
         __strake_send_message('https:' + (typeof require('https').request === 'function') + '/' + (require('node:https').globalAgent instanceof require('https').Agent)); \
         const agent = new http.Agent({ keepAlive: true, maxSockets: 5 }); \
         const req = http.request({ host: '127.0.0.1', port: PORT, path: '/x?q=1', method: 'POST', headers: { 'x-a': 'b' }, agent, timeout: 5000 }, (res) => { \
           __strake_send_message('status:' + res.statusCode + '/' + res.statusMessage + '/' + res.headers['x-echo']); \
           let text = ''; \
           res.on('end', () => __strake_send_message('body:' + text)); \
           res.on('data', (c) => { text += c.toString(); }); \
         }); \
         __strake_send_message('shape:' + (req instanceof http.ClientRequest) + '/' + (typeof req.setHeader === 'function')); \
         req.on('error', (e) => __strake_send_message('error:' + e.code)); \
         req.setHeader('x-req', 'yes'); \
         req.end('ping'); \
         http.get({ host: '127.0.0.1', port: PORT, path: '/g' }, (res) => { \
           res.on('end', () => __strake_send_message('get:' + res.statusCode)); \
           res.resume(); \
         }).on('error', (e) => __strake_send_message('get-error:' + e.code)); \
         const srv = http.createServer((req, res) => {}); \
         let listenCode = 'none'; \
         try { srv.listen(0); } catch (e) { listenCode = e.code; } \
         __strake_send_message('listen:' + listenCode); \
         http.request({ host: '127.0.0.1', port: 1, path: '/' }, () => {}).on('error', (e) => __strake_send_message('refused:' + e.code)).end();"
        .replace("PORT", &port.to_string());
    doc.eval(&eval_js);
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "http must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "agent:true/true",
            "https:true/true",
            "shape:true/true",
            "listen:ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
            "status:201/Created/got-it",
            "body:pong",
            "get:201",
            "refused:ECONNREFUSED",
        ]
    );
    // The fixture saw a real POST with the custom header and body.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let raw = seen.lock().expect("http fixture lock").clone();
        if raw.windows(4).any(|w| w == b"ping") || Instant::now() >= deadline {
            let text = String::from_utf8_lossy(&raw);
            assert!(
                text.starts_with("POST /x?q=1 HTTP/1.1"),
                "fixture saw POST line, got {text:?}"
            );
            assert!(
                text.contains("x-req: yes"),
                "fixture saw header, got {text:?}"
            );
            assert!(text.ends_with("ping"), "fixture saw body, got {text:?}");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Issue #155: `string_decoder` — Joplin's streaming parsers (`sax`,
/// tar) hold a `new StringDecoder("utf8")` across `write()` calls, so split
/// multi-byte sequences must survive chunk boundaries and flush as U+FFFD.
#[test]
fn node_string_decoder_buffers_splits() {
    let mut doc = loader_doc_at(&fs_temp_dir("strdec"));
    doc.eval(
        "const { StringDecoder } = require('string_decoder'); \
         __strake_send_message('prefix:' + (require('node:string_decoder').StringDecoder === StringDecoder)); \
         const d = new StringDecoder('utf8'); \
         const a = d.write(Buffer.from([0x68, 0xc3])); \
         const b = d.write(Buffer.from([0xa9, 0x21])); \
         __strake_send_message('split:' + a + '/' + b + '/' + d.end()); \
         __strake_send_message('flush:' + new StringDecoder('utf8').end(Buffer.from([0xc3]))); \
         const d16 = new StringDecoder('utf16le'); \
         __strake_send_message('u16:' + d16.write(Buffer.from([0x41, 0x00, 0x42, 0x00])) + d16.end()); \
         __strake_send_message('latin:' + new StringDecoder('latin1').write(Buffer.from([65, 233]))); \
         __strake_send_message('b64:' + new StringDecoder('base64').write('aGk=')); \
         __strake_send_message('hex:' + new StringDecoder('hex').write('dead')); \
         __strake_send_message('def:' + new StringDecoder().write(Buffer.from([0x7a]))); \
         let code = 'none'; \
         try { new StringDecoder('rot13'); } catch (e) { code = e.code; } \
         __strake_send_message('unknown:' + code);",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "string_decoder must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "prefix:true",
            "split:h/\u{e9}!/",
            "flush:\u{fffd}",
            "u16:AB",
            "latin:A\u{e9}",
            "b64:hi",
            "hex:\u{de}\u{ad}",
            "def:z",
            "unknown:ERR_UNKNOWN_ENCODING",
        ]
    );
}

/// Issue #155: `net`/`tls` shapes — Joplin's port probe (`new
/// net.Socket` + `connect`/`unref`) needs real connect-vs-refused truth,
/// `agentkeepalive` aliases `createConnection` at import, and a Node-compat
/// shim calls `tls.createSecureContext()` at import. Duplex I/O stays
/// follow-up work (coded throws); the probe never transfers bytes.
#[test]
fn node_net_socket_shapes() {
    use std::time::{Duration, Instant};
    // Fixture (test-only): accept one connection, then close without
    // sending — the probe's port-check pattern in miniature.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("net fixture binds localhost");
    let port = listener.local_addr().expect("net fixture port").port();
    listener
        .set_nonblocking(true)
        .expect("net fixture nonblocking");
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    drop(stream);
                    return;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    });
    let mut doc = loader_doc_at(&fs_temp_dir("netshape"));
    let eval_js = "const net = require('net'); \
         __strake_send_message('ip:' + net.isIP('127.0.0.1') + '/' + net.isIP('::1') + '/' + net.isIP('nope') + '/' + net.isIPv4('1.2.3.4') + '/' + net.isIPv6('::1')); \
         __strake_send_message('tls:' + (typeof require('tls').createSecureContext() === 'object')); \
         const s = new net.Socket(); \
         __strake_send_message('shape:' + (s instanceof net.Socket) + '/' + (typeof net.createConnection === 'function')); \
         s.once('connect', () => { __strake_send_message('connected'); s.end(); s.destroy(); }); \
         s.once('error', (e) => __strake_send_message('error:' + e.code)); \
         s.connect({ host: '127.0.0.1', port: PORT }); \
         s.unref(); \
         net.connect({ host: '127.0.0.1', port: 1 }).once('error', (e) => __strake_send_message('refused:' + e.code)); \
         let tlsCode = 'none'; \
         try { require('tls').connect({}); } catch (e) { tlsCode = e.code; } \
         __strake_send_message('tls-connect:' + tlsCode); \
         const srv = net.createServer(() => {}); \
         let listenCode = 'none'; \
         try { srv.listen(0); } catch (e) { listenCode = e.code; } \
         __strake_send_message('listen:' + listenCode);"
        .replace("PORT", &port.to_string());
    doc.eval(&eval_js);
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "net must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "ip:4/6/0/true/true",
            "tls:true",
            "shape:true/true",
            "tls-connect:ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
            "listen:ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
            "connected",
            "refused:ECONNREFUSED",
        ]
    );
}

/// Issue #155: `domain` — Sentry reads `domain.active` and falls back to
/// `domain.create()` + `bind()` as its async-context carrier. `run` executes
/// synchronously with `enter`/`exit` (so `active` is real inside `run`);
/// async continuation tracking is out of scope, like every other shim here.
#[test]
fn node_domain_carrier_shapes() {
    let mut doc = loader_doc_at(&fs_temp_dir("domainshape"));
    doc.eval(
        "const domain = require('domain'); \
         __strake_send_message('idle:' + (domain.active === undefined || domain.active === null)); \
         __strake_send_message('prefix:' + (require('node:domain').create === domain.create)); \
         const d = domain.create(); \
         d.on('error', (e) => __strake_send_message('caught:' + e.message)); \
         let ran = ''; \
         d.run(() => { ran += 'a'; }); \
         __strake_send_message('run:' + ran + '/' + (domain.active === undefined || domain.active === null)); \
         d.run(() => { throw new Error('boom'); }); \
         __strake_send_message('bind:' + d.bind((x) => 'b' + x)('!')); \
         d.run(() => __strake_send_message('inside:' + (domain.active === d)));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "domain must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "idle:true",
            "prefix:true",
            "run:a/true",
            "caught:boom",
            "bind:b!",
            "inside:true",
        ]
    );
}

/// Issue #155: `async_hooks.AsyncLocalStorage` — Sentry's async-context
/// strategy runs hubs via `als.run(store, fn)` and reads `getStore()`.
/// Synchronous propagation is real (nesting restores); cross-microtask
/// tracking needs true async resources (out of scope, like `domain`).
#[test]
fn node_async_local_storage_sync_scope() {
    let mut doc = loader_doc_at(&fs_temp_dir("alscope"));
    doc.eval(
        "const { AsyncLocalStorage } = require('async_hooks'); \
         __strake_send_message('prefix:' + (require('node:async_hooks').AsyncLocalStorage === AsyncLocalStorage)); \
         const als = new AsyncLocalStorage(); \
         __strake_send_message('empty:' + (als.getStore() === undefined)); \
         __strake_send_message('run:' + als.run('hub1', () => als.getStore())); \
         __strake_send_message('after:' + (als.getStore() === undefined)); \
         als.run('hub2', () => { \
           __strake_send_message('nested:' + als.getStore()); \
           als.run('hub3', () => __strake_send_message('inner:' + als.getStore())); \
           __strake_send_message('restored:' + als.getStore()); \
         }); \
         let threw = 'none'; \
         try { als.run('hub4', () => { throw new Error('als-boom'); }); } catch (e) { threw = e.message; } \
         __strake_send_message('throw:' + threw + '/' + (als.getStore() === undefined));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "als must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "prefix:true",
            "empty:true",
            "run:hub1",
            "after:true",
            "nested:hub2",
            "inner:hub3",
            "restored:hub2",
            "throw:als-boom/true",
        ]
    );
}

/// Issue #155: `util.promisify` — Joplin promisifies `fs.readFile`/
/// `fs.readdir` at import time. The wrapper returns real promises (never
/// sync values); rejections carry the sync error codes.
#[test]
fn node_util_promisify_wraps_callbacks() {
    let root = fs_temp_dir("promisify");
    let mut doc = fs_doc_at(&root);
    let script = "const util = require('util'); \
         const fs = require('fs'); \
         __strake_send_message('custom:' + (typeof util.promisify.custom === 'symbol')); \
         const readFile = util.promisify(fs.readFile); \
         const pending = readFile(DIR_PLACEHOLDER + '/x.txt', 'utf8'); \
         __strake_send_message('promise:' + (pending instanceof Promise)); \
         pending
           .then((text) => __strake_send_message('read:' + text))
           .then(() => util.promisify(fs.readdir)(DIR_PLACEHOLDER))
           .then((entries) => __strake_send_message('ls:' + entries.sort().join(',')))
           .then(() => readFile(DIR_PLACEHOLDER + '/missing.txt'))
           .then(
             () => __strake_send_message('unexpected:resolved'),
             (e) => __strake_send_message('code:' + e.code)
           );"
    .replace("DIR_PLACEHOLDER", &format!("'{}'", js_path(&root)));
    std::fs::write(root.join("x.txt"), "promisified").expect("promisify fixture writes");
    doc.eval(&script);
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "promisify must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "custom:true",
            "promise:true",
            "read:promisified",
            "ls:x.txt",
            "code:ENOENT"
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #155: `node:tty` surface — feature sniffers (`supports-color`)
/// call `isatty(1/2)` at import time. Headless boot has no terminal, so
/// `isatty` honestly reports `false` (colors off, plain logs).
#[test]
fn node_tty_headless_reports_no_terminal() {
    let mut doc = loader_doc_at(&fs_temp_dir("tty"));
    doc.eval(
        "const tty = require('tty'); \
         __strake_send_message('isatty:' + tty.isatty(1) + '/' + tty.isatty(2)); \
         const ws = new tty.WriteStream(1); \
         __strake_send_message('ws:' + ws.isTTY + '/' + ws.getWindowSize().join('x')); \
         let code = 'none'; \
         try { ws.setRawMode(true); } catch (e) { code = e.code; } \
         __strake_send_message('raw:' + code); \
         __strake_send_message('same:' + (require('node:tty') === tty));",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "tty must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "isatty:false/false",
            "ws:false/80x24",
            "raw:ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
            "same:true",
        ]
    );
}

/// Issue #155: `util.deprecate` wraps with warn-once semantics — the
/// `debug` package calls it at import time, so its absence blocks boot.
#[test]
fn node_util_deprecate_wraps_once() {
    let mut doc = loader_doc_at(&fs_temp_dir("deprecate"));
    doc.eval(
        "const util = require('util'); \
         __strake_send_message('typeof:' + (typeof util.deprecate)); \
         const seen = []; \
         const fn = util.deprecate((x) => { seen.push(x); return x * 2; }, 'old fn', 'DEP0001'); \
         __strake_send_message('wrapped:' + (fn(21) + '/' + fn(22) + '/' + seen.join(',')));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "deprecate must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["typeof:function", "wrapped:42/44/21,22"]
    );
}

/// Issue #155: `node:stream` class family — real updaters `extend Transform`
/// and deliver via the `_transform` callback's data argument (never pushing
/// manually), so the callback arg must flow downstream.
#[test]
fn node_stream_class_family_flows() {
    let mut doc = loader_doc_at(&fs_temp_dir("streams2"));
    doc.eval(
        "const stream = require('stream'); \
         __strake_send_message('names:' + ['Readable', 'Writable', 'Duplex', 'Transform', 'PassThrough', 'pipeline'].map((k) => typeof stream[k]).join(',')); \
         class Upper extends stream.Transform { \
           _transform(chunk, enc, cb) { cb(null, String(chunk).toUpperCase()); } \
         } \
         const seen = []; \
         const t = new Upper(); \
         t.on('data', (c) => seen.push(String(c))); \
         t.on('end', () => __strake_send_message('t:' + seen.join(''))); \
         t.write('a'); \
         t.end('b'); \
         const collected = []; \
         const dest = new stream.Writable({ write(c, e, cb) { collected.push(String(c)); cb(); } }); \
         stream.pipeline(stream.Readable.from(['x', 'y']), new stream.PassThrough(), dest, (err) => { \
           __strake_send_message('pipe:' + (err ? err.code : collected.join(''))); \
         });",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "streams must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec![
            "names:function,function,function,function,function,function",
            "t:AB",
            "pipe:xy",
        ]
    );
}

/// Issue #155: `process` stdio handles — feature sniffers read
/// `process.stderr.fd` at import time. Headless handles carry the standard
/// fds/dimensions and route writes to the console.
#[test]
fn node_process_stdio_handles() {
    let mut doc = loader_doc_at(&fs_temp_dir("stdio"));
    doc.eval(
        "const tty = require('tty'); \
         __strake_send_message('stdio:' + process.stdout.fd + '/' + process.stderr.fd + '/' + process.stdin.fd + '/' + process.stdout.isTTY); \
         __strake_send_message('instance:' + (process.stdout instanceof tty.WriteStream)); \
         process.stdout.write('hello-stdout'); \
         process.stderr.write('hello-stderr'); \
         __strake_send_message('size:' + process.stdout.columns + 'x' + process.stdout.rows);",
    );
    let errors = doc.take_js_errors();
    assert!(errors.is_empty(), "stdio must not throw, got {errors:?}");
    assert_eq!(
        doc.take_messages(),
        vec!["stdio:1/2/0/false", "instance:true", "size:80x24"]
    );
}

/// Issue #154: renderer parity follows real Electron defaults — no `fs`
/// without node integration, but the pure modules (`buffer`, `util`,
/// `stream`, `constants`) resolve everywhere.
#[test]
fn node_fs_unavailable_in_renderer() {
    let host = ElectronHost::new("RendererFs", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron_renderer(&host);
    assert!(
        doc.take_js_errors().is_empty(),
        "renderer bootstrap must install cleanly"
    );
    doc.eval(
        "let fsCode = 'none'; \
         try { require('fs'); } catch (e) { fsCode = String(e.message).includes('Cannot find module') ? 'missing' : 'other:' + e.message; } \
         __strake_send_message('fs:' + fsCode); \
         __strake_send_message('buffer:' + (require('buffer').Buffer === globalThis.Buffer)); \
         __strake_send_message('util:' + require('util').format('%s!', 'hi'));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "renderer requires must not throw uncaught, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["fs:missing", "buffer:true", "util:hi!"]
    );
}

/// `process.type` marks the Electron context flavor (issue #155): main
/// contexts report `browser` so main-entry guards (`@sentry/electron`)
/// take the main branch, while renderer contexts report `renderer`.
#[test]
fn process_type_marks_main_and_renderer_contexts() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval("__strake_send_message('main-type:' + process.type);");
    assert!(
        doc.take_js_errors().is_empty(),
        "reading process.type must not throw"
    );
    assert_eq!(doc.take_messages(), vec!["main-type:browser"]);

    let host = ElectronHost::new("RendererType", "1.0.0");
    let mut renderer =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    renderer.install_electron_renderer(&host);
    assert!(
        renderer.take_js_errors().is_empty(),
        "renderer bootstrap must install cleanly"
    );
    renderer.eval("__strake_send_message('renderer-type:' + process.type);");
    assert!(
        renderer.take_js_errors().is_empty(),
        "reading process.type must not throw"
    );
    assert_eq!(renderer.take_messages(), vec!["renderer-type:renderer"]);
}

/// Main-process contexts expose no DOM globals (issue #155): real
/// Electron main has no `window`/`document`, and entry-point guards
/// (`@sentry/electron`: `typeof window < "u" ? "renderer" : "main"`) take
/// the renderer branch when they exist. Renderers keep them.
#[test]
fn main_hides_dom_globals_while_renderer_keeps_them() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "__strake_send_message('main-window:' + typeof window); \
         __strake_send_message('main-document:' + typeof document);",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "typeof probes must not throw"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["main-window:undefined", "main-document:undefined"]
    );

    let host = ElectronHost::new("RendererDom", "1.0.0");
    let mut renderer =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    renderer.install_electron_renderer(&host);
    assert!(
        renderer.take_js_errors().is_empty(),
        "renderer bootstrap must install cleanly"
    );
    renderer.eval("__strake_send_message('renderer-window:' + typeof window);");
    assert!(
        renderer.take_js_errors().is_empty(),
        "typeof probe must not throw"
    );
    assert_eq!(renderer.take_messages(), vec!["renderer-window:object"]);
}

/// Global `performance` (issue #155): Node 16+ and Electron main expose
/// `performance.now()`; Joplin's Sentry init calls it during boot.
#[test]
fn performance_now_measures_elapsed_milliseconds() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "__strake_send_message('perf-type:' + typeof performance); \
         __strake_send_message('perf-now:' + (typeof performance.now() === 'number' && performance.now() >= 0)); \
         __strake_send_message('perf-origin:' + (typeof performance.timeOrigin === 'number'));",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "performance probes must not throw"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["perf-type:object", "perf-now:true", "perf-origin:true",]
    );
}

/// `node:timers` / bare `timers` (issue #155): the module re-exports the
/// runtime timer globals Joplin's bundle requires bare.
#[test]
fn timers_module_exposes_runtime_timer_globals() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const timersMod = require('timers'); \
         __strake_send_message('timers-setTimeout:' + (timersMod.setTimeout === setTimeout)); \
         __strake_send_message('timers-clearTimeout:' + (timersMod.clearTimeout === clearTimeout)); \
         __strake_send_message('timers-setInterval:' + (timersMod.setInterval === setInterval)); \
         __strake_send_message('timers-clearInterval:' + (timersMod.clearInterval === clearInterval)); \
         __strake_send_message('node-timers:' + (require('node:timers') === timersMod));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "timers requires must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "timers-setTimeout:true",
            "timers-clearTimeout:true",
            "timers-setInterval:true",
            "timers-clearInterval:true",
            "node-timers:true",
        ]
    );
}

/// `node:dgram` / bare `dgram` (issue #155): Joplin's shim registry
/// requires it at load but only touches it through a lazy accessor, so the
/// require succeeds while socket creation throws a coded error instead of
/// silently dropping datagrams.
#[test]
fn dgram_require_succeeds_but_sockets_are_coded_unavailable() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const dgramMod = require('dgram'); \
         __strake_send_message('dgram-type:' + typeof dgramMod.createSocket); \
         __strake_send_message('node-dgram:' + (require('node:dgram') === dgramMod)); \
         try { dgramMod.createSocket('udp4'); __strake_send_message('dgram-create:NO-THROW'); } \
         catch (e) { __strake_send_message('dgram-create:' + e.code); }",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "dgram probes must not throw uncaught, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "dgram-type:function",
            "node-dgram:true",
            "dgram-create:ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ]
    );
}

/// `node:dns` / bare `dns` (issue #155): Joplin's CLI layer requires it at
/// load and conditionally calls `setDefaultResultOrder("ipv4first")`.
/// Result-order state is real (validated setter, `verbatim` default);
/// actual resolution needs a datagram transport and stays out of scope.
#[test]
fn dns_default_result_order_round_trips_and_validates() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const dnsMod = require('dns'); \
         __strake_send_message('dns-default:' + dnsMod.getDefaultResultOrder()); \
         __strake_send_message('node-dns:' + (require('node:dns') === dnsMod)); \
         dnsMod.setDefaultResultOrder('ipv4first'); \
         __strake_send_message('dns-ipv4first:' + dnsMod.getDefaultResultOrder()); \
         try { dnsMod.setDefaultResultOrder('bogus'); __strake_send_message('dns-bogus:NO-THROW'); } \
         catch (e) { __strake_send_message('dns-bogus:' + e.code); }",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "dns probes must not throw uncaught, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "dns-default:verbatim",
            "node-dns:true",
            "dns-ipv4first:ipv4first",
            "dns-bogus:ERR_INVALID_ARG_VALUE",
        ]
    );
}

/// `node:http2` / bare `http2` (issue #155): Joplin's sync stack requires
/// it at load and uses `constants` pseudo-headers plus `connect()` at
/// request time. Constants are real (RFC 7540); sessions stay
/// coded-unavailable (no HTTP/2 transport).
#[test]
fn http2_constants_are_real_while_sessions_are_coded_unavailable() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const http2Mod = require('http2'); \
         __strake_send_message('http2-path:' + http2Mod.constants.HTTP2_HEADER_PATH); \
         __strake_send_message('http2-method:' + http2Mod.constants.HTTP2_HEADER_METHOD); \
         __strake_send_message('node-http2:' + (require('node:http2') === http2Mod)); \
         try { http2Mod.connect('https://example.com'); __strake_send_message('http2-connect:NO-THROW'); } \
         catch (e) { __strake_send_message('http2-connect:' + e.code); }",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "http2 probes must not throw uncaught, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "http2-path::path",
            "http2-method::method",
            "node-http2:true",
            "http2-connect:ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ]
    );
}

/// `app.setName` (issue #155): Joplin renames itself at load; the new
/// name reports back through `getName` on every `require('electron')`.
#[test]
fn app_set_name_updates_get_name() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const eName = require('electron'); \
         __strake_send_message('before:' + eName.app.getName()); \
         eName.app.setName('Joplin'); \
         __strake_send_message('after:' + eName.app.getName()); \
         __strake_send_message('fresh:' + require('electron').app.getName());",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "setName probes must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["before:QuickStart", "after:Joplin", "fresh:Joplin"]
    );
}

/// `app.setAsDefaultProtocolClient` (issue #155): records the protocol
/// and reports success like Electron (OS handler effect stays deferred).
#[test]
fn app_set_as_default_protocol_client_reports_success() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "__strake_send_message('set-proto:' + require('electron').app.setAsDefaultProtocolClient('joplin'));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "setAsDefaultProtocolClient must not throw, got {errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["set-proto:true"]);
}

/// `app.setAppUserModelId` (issue #155): Joplin sets it right after the
/// profile mkdir; headless has no taskbar integration so the id is
/// recorded and the call returns undefined like Electron off-Windows.
#[test]
fn app_set_app_user_model_id_is_recorded() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "__strake_send_message('set-id:' + require('electron').app.setAppUserModelId('net.cozic.joplin-desktop'));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "setAppUserModelId must not throw, got {errors:?}"
    );
    assert_eq!(doc.take_messages(), vec!["set-id:undefined"]);
}

/// `protocol.registerSchemesAsPrivileged` (issue #155): Joplin declares
/// its custom schemes at load; entries validate, extras are TypeErrors.
#[test]
fn protocol_register_schemes_as_privileged_validates_and_records() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const eProto = require('electron'); \
         eProto.protocol.registerSchemesAsPrivileged([{ scheme: 'joplin-content', privileges: { standard: true, secure: true } }]); \
         __strake_send_message('register:ok'); \
         try { eProto.protocol.registerSchemesAsPrivileged([{ privileges: {} }]); __strake_send_message('missing:NO-THROW'); } \
         catch (e) { __strake_send_message('missing:' + (e instanceof TypeError)); } \
         try { eProto.protocol.registerSchemesAsPrivileged('nope'); __strake_send_message('array:NO-THROW'); } \
         catch (e) { __strake_send_message('array:' + (e instanceof TypeError)); }",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "protocol probes must not throw uncaught, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["register:ok", "missing:true", "array:true"]
    );
}

/// `util.TextEncoder`/`TextDecoder` (issue #155): Node re-exports the
/// globals from `util`; Sentry's `NodeClient` news one up at construction.
#[test]
fn util_text_coders_match_the_globals() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const utilCoders = require('util'); \
         __strake_send_message('enc:' + (utilCoders.TextEncoder === TextEncoder)); \
         __strake_send_message('dec:' + (utilCoders.TextDecoder === TextDecoder)); \
         __strake_send_message('roundtrip:' + (new utilCoders.TextDecoder().decode(new utilCoders.TextEncoder().encode('joplin')) === 'joplin'));",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "util coder probes must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["enc:true", "dec:true", "roundtrip:true"]
    );
}

/// `process` is an EventEmitter (issue #155): Joplin installs an
/// `unhandledRejection` handler at load via `process.on(...)`.
#[test]
fn process_supports_on_emit_once_like_an_event_emitter() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "__strake_send_message('process-on:' + (typeof process.on)); \
         let procFired = null; \
         process.on('probe-event', (v) => { procFired = v; }); \
         process.emit('probe-event', 42); \
         __strake_send_message('process-emit:' + procFired); \
         process.once('once-event', (v) => __strake_send_message('process-once:' + v)); \
         process.emit('once-event', 7); \
         process.emit('once-event', 8);",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "process emitter probes must not throw, got {errors:?}"
    );
    assert_eq!(
        doc.take_messages(),
        vec!["process-on:function", "process-emit:42", "process-once:7",]
    );
}

/// `app.getAppPath`/`getPath`/`setPath` (issue #155): the app root and the
/// always-known `temp` path resolve to real directories, `userData` throws
/// until the embedder sets it, the set round-trips, and unknown names throw
/// instead of returning an empty string.
#[test]
fn app_paths_resolve_set_and_throw_like_electron() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const ePaths = require('electron'); \
         __strake_send_message('app-path:' + ePaths.app.getAppPath()); \
         __strake_send_message('temp:' + ePaths.app.getPath('temp')); \
         __strake_send_message('userdata-default:' + ePaths.app.getPath('userData')); \
         ePaths.app.setPath('userData', '/tmp/strake-test-profile'); \
         __strake_send_message('userdata:' + ePaths.app.getPath('userData')); \
         try { ePaths.app.getPath('bogus-name'); __strake_send_message('bogus:NO-THROW'); } \
         catch (e) { __strake_send_message('bogus:' + e.message); }",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "path probes must not throw uncaught, got {errors:?}"
    );
    let messages = doc.take_messages();
    assert_eq!(
        messages.len(),
        5,
        "expected five path probes, got {messages:?}"
    );
    for key in ["app-path:", "temp:", "userdata-default:"] {
        let hit = messages.iter().find(|m| m.starts_with(key));
        assert!(
            hit.is_some_and(|m| m.len() > key.len()),
            "{key} must resolve to a non-empty directory, got {messages:?}"
        );
    }
    assert!(
        messages.iter().any(|m| m
            .strip_prefix("userdata-default:")
            .is_some_and(|path| path.ends_with("QuickStart"))),
        "unset userData must default to appData/<name>, got {messages:?}"
    );
    assert!(
        messages.contains(&"userdata:/tmp/strake-test-profile".to_string()),
        "setPath must round-trip through getPath, got {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("bogus:") && m.contains("bogus-name")),
        "unknown path names must throw naming the name, got {messages:?}"
    );
}

/// Joplin boot slice (issue #155): `session.fromPath` + `protocol.handle`
/// + `webContents.session.webRequest` must not throw and must record.
#[test]
fn joplin_session_slice_records_protocol_and_webrequest() {
    let host = ElectronHost::new("Joplin", "3.4.12");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    assert!(
        doc.take_js_errors().is_empty(),
        "electron bootstrap must install cleanly"
    );
    doc.eval(
        "const { app, session, BrowserWindow } = require('electron'); \
         const s = session.fromPath('/tmp/joplin-profile/internal', { cache: false }); \
         __strake_send_message('session-ok:' + (typeof s.protocol.handle)); \
         s.protocol.handle('joplin-content', (req) => {}); \
         s.protocol.handle('joplin-plugin', (req) => {}); \
         try { s.protocol.handle('joplin-content', 'nope'); __strake_send_message('handle-nonfn:NO-THROW'); } \
         catch (e) { __strake_send_message('handle-nonfn:' + (e instanceof TypeError)); } \
         const dflt = session.defaultSession; \
         __strake_send_message('default:' + (typeof dflt.protocol.handle)); \
         dflt.webRequest.onHeadersReceived((details) => {}); \
         const s2 = session.fromPartition('electron-updater', { cache: false }); \
         __strake_send_message('partition:' + (typeof s2.webRequest.onBeforeSendHeaders)); \
         const win = new BrowserWindow({ width: 800, height: 600, webPreferences: { session: s } }); \
         __strake_send_message('wc-session:' + (win.webContents.session === s)); \
         win.webContents.session.webRequest.onBeforeSendHeaders({ urls: ['*://*.youtube.com/*'] }, (d, u) => {}); \
         __strake_send_message('webrequest-ok:1'); \
         const win2 = new BrowserWindow({ width: 100, height: 100 }); \
         __strake_send_message('wc-default:' + (win2.webContents.session === dflt));",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "session slice must evaluate without throwing"
    );
    let messages = doc.take_messages();
    for expected in [
        "session-ok:function",
        "handle-nonfn:true",
        "default:function",
        "partition:function",
        "wc-session:true",
        "webrequest-ok:1",
        "wc-default:true",
    ] {
        assert!(
            messages.contains(&expected.to_string()),
            "expected probe message {expected:?}, got {messages:?}"
        );
    }

    // Three sessions: default (id 0), the fromPath profile, the updater partition.
    let ids = host.session_ids();
    assert_eq!(ids.len(), 3, "expected three sessions, got {ids:?}");
    assert!(ids.contains(&0));
    let path_session = *ids.iter().find(|id| **id != 0).expect("profile session");
    assert_eq!(
        host.session_handled_schemes(path_session),
        vec!["joplin-content", "joplin-plugin"],
        "protocol.handle must record both Joplin schemes"
    );
    assert_eq!(
        host.session_web_request_rules(path_session),
        vec![(
            "on-before-send-headers".to_string(),
            vec!["*://*.youtube.com/*".to_string()]
        )],
        "webRequest filter+listener must be recorded"
    );
    assert_eq!(
        host.session_web_request_rules(0),
        vec![("on-headers-received".to_string(), Vec::new())],
        "no-filter onHeadersReceived must be recorded on the default session"
    );
    assert_eq!(
        host.window_session(0),
        Some(path_session),
        "webPreferences.session must bind the window to the Joplin session"
    );
    assert_eq!(
        host.window_session(1),
        Some(0),
        "a window without webPreferences.session uses the default session"
    );
}

/// `nativeTheme` (issue #155): Joplin reads `shouldUseDarkColors` for the
/// window background literal before `new BrowserWindow`; the module must
/// exist with Electron's system-follow default plus a settable source.
#[test]
fn native_theme_reports_system_default_and_accepts_source() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const { nativeTheme } = require('electron'); \
         __strake_send_message('dark:' + nativeTheme.shouldUseDarkColors); \
         __strake_send_message('source:' + nativeTheme.themeSource); \
         nativeTheme.themeSource = 'dark'; \
         __strake_send_message('dark-after:' + nativeTheme.shouldUseDarkColors); \
         __strake_send_message('source-after:' + nativeTheme.themeSource); \
         nativeTheme.themeSource = 'light'; \
         __strake_send_message('dark-light:' + nativeTheme.shouldUseDarkColors); \
         try { nativeTheme.themeSource = 'neon'; __strake_send_message('bogus:NO-THROW'); } \
         catch (e) { __strake_send_message('bogus:' + (e instanceof TypeError)); }",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "nativeTheme probes must not throw uncaught"
    );
    let messages = doc.take_messages();
    for expected in [
        "dark:false",
        "source:system",
        "dark-after:true",
        "source-after:dark",
        "dark-light:false",
        "bogus:true",
    ] {
        assert!(
            messages.contains(&expected.to_string()),
            "expected probe message {expected:?}, got {messages:?}"
        );
    }
}

/// `webContents.on` (issue #155): Joplin subscribes to `unresponsive` (and
/// later load events) right after `createWindow`; subscriptions must record
/// per window instead of throwing "not a callable function".
#[test]
fn webcontents_on_records_event_listeners() {
    let (mut doc, host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const { BrowserWindow: BW } = require('electron'); \
         const win = new BW({ width: 800, height: 600 }); \
         const chained = win.webContents.on('unresponsive', () => {}); \
         __strake_send_message('chain:' + (chained === win.webContents)); \
         win.webContents.on('did-finish-load', () => {}); \
         try { win.webContents.on('crashed', 'nope'); __strake_send_message('nonfn:NO-THROW'); } \
         catch (e) { __strake_send_message('nonfn:' + (e instanceof TypeError)); }",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "webContents.on probes must not throw uncaught, got {errors:?}"
    );
    let messages = doc.take_messages();
    for expected in ["chain:true", "nonfn:true"] {
        assert!(
            messages.contains(&expected.to_string()),
            "expected probe message {expected:?}, got {messages:?}"
        );
    }
    assert_eq!(
        host.web_contents_listener_events(0),
        vec!["did-finish-load", "unresponsive"],
        "both webContents subscriptions must be recorded"
    );
}

/// `webContents.setWindowOpenHandler` (issue #155): Joplin overrides
/// `window.open` handling during `createWindow`; the handler must record
/// instead of throwing "not a callable function".
#[test]
fn webcontents_set_window_open_handler_records() {
    let (mut doc, host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const { BrowserWindow: BW2 } = require('electron'); \
         const win = new BW2({ width: 800, height: 600 }); \
         win.webContents.setWindowOpenHandler(() => ({ action: 'deny' })); \
         __strake_send_message('handler:set'); \
         try { win.webContents.setWindowOpenHandler('nope'); __strake_send_message('nonfn:NO-THROW'); } \
         catch (e) { __strake_send_message('nonfn:' + (e instanceof TypeError)); }",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "setWindowOpenHandler probes must not throw uncaught, got {errors:?}"
    );
    let messages = doc.take_messages();
    for expected in ["handler:set", "nonfn:true"] {
        assert!(
            messages.contains(&expected.to_string()),
            "expected probe message {expected:?}, got {messages:?}"
        );
    }
    assert!(
        host.web_contents_has_window_open_handler(0),
        "the window.open handler must be recorded"
    );
}

/// `win.hide()` (issue #155): Joplin starts hidden (`show: false`) and
/// hides explicitly; visibility must flip like `show`, silently ignoring
/// destroyed ids like `show`/`close` do.
#[test]
fn window_hide_flips_visibility() {
    let (mut doc, host) = main_doc();
    doc.take_messages();
    doc.eval(
        "const { BrowserWindow: BW3 } = require('electron'); \
         const hidden = new BW3({ width: 100, height: 100, show: false }); \
         __strake_send_message('hidden:' + hidden.isVisible()); \
         const win = new BW3({ width: 100, height: 100 }); \
         __strake_send_message('shown:' + win.isVisible()); \
         win.hide(); \
         __strake_send_message('after-hide:' + win.isVisible()); \
         win.show(); \
         __strake_send_message('after-show:' + win.isVisible());",
    );
    let errors = doc.take_js_errors();
    assert!(
        errors.is_empty(),
        "hide probes must not throw uncaught, got {errors:?}"
    );
    let messages = doc.take_messages();
    for expected in [
        "hidden:false",
        "shown:true",
        "after-hide:false",
        "after-show:true",
    ] {
        assert!(
            messages.contains(&expected.to_string()),
            "expected probe message {expected:?}, got {messages:?}"
        );
    }
    assert!(
        !host.window_visible(0),
        "hide must clear visibility in core"
    );
}

/// `path` drive/backslash semantics (issue #155): on Windows,
/// `path.join(__dirname, "..", ...)` must escape the dir instead of
/// collapsing to a relative path (which silently kept outside-grant
/// writes inside the grant, hiding the EACCES the grants test asserts).
/// Win32 branches run on Windows CI; POSIX branches everywhere else.
#[test]
fn path_dotdot_resolves_across_platforms() {
    let (mut doc, _host) = main_doc();
    doc.take_messages();
    if cfg!(windows) {
        doc.eval(
            "const pw = require('node:path'); \
             __strake_send_message('join:' + pw.join('C:\\\\app', '..', 'profile')); \
             __strake_send_message('abs:' + pw.isAbsolute('C:\\\\app')); \
             __strake_send_message('rel:' + pw.isAbsolute('app\\\\x')); \
             __strake_send_message('dir:' + pw.dirname('C:\\\\app\\\\main.js')); \
             __strake_send_message('base:' + pw.basename('C:\\\\app\\\\main.js'));",
        );
        let messages = doc.take_messages();
        assert_eq!(
            messages,
            vec![
                "join:C:/profile",
                "abs:true",
                "rel:false",
                "dir:C:/app",
                "base:main.js",
            ],
            "win32 join/dirname must resolve drives, got {messages:?}"
        );
    } else {
        doc.eval(
            "const px = require('node:path'); \
             __strake_send_message('join:' + px.join('/app', '..', 'profile')); \
             __strake_send_message('abs:' + px.isAbsolute('/app'));",
        );
        let messages = doc.take_messages();
        assert_eq!(
            messages,
            vec!["join:/profile", "abs:true"],
            "posix join must keep resolving .., got {messages:?}"
        );
    }
    assert!(
        doc.take_js_errors().is_empty(),
        "path probes must not throw"
    );
}
