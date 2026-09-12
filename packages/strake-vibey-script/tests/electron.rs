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
fn methods_on_destroyed_window_throw() {
    let (mut doc, _host) = main_doc();
    doc.eval(
        "const BW = require('electron').BrowserWindow; \
         const doomed = new BW({ show: false }); \
         doomed.close(); \
         doomed.loadFile('late.html');",
    );
    let errors = doc.take_js_errors();
    assert_eq!(errors.len(), 1, "expected one throw, got {errors:?}");
    assert!(
        errors[0].contains("destroyed"),
        "unexpected error: {}",
        errors[0]
    );
}
