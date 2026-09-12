//! Slice 3 conformance (issue #83): a renderer page executes against the DOM
//! engine with `require('electron').ipcRenderer`, and `invoke`/`send` calls
//! round-trip through main-process `ipcMain` handlers via `pump_ipc`.
//! Headless: the pump is the in-process analogue of Electron's cross-process
//! transport (zero-copy shared memory arrives with issue #15).

use strake_dom::{Document, DocumentConfig};
use strake_vibey_script::{ElectronHost, ScriptDocument};

const MAIN_JS: &str = r#"
const { app, ipcMain } = require('electron');
app.whenReady().then(() => {
    ipcMain.handle('ping', (event, msg) => 'pong:' + msg);
    ipcMain.handle('add', (event, a, b) => a + b);
    ipcMain.handle('boom', () => { throw new Error('kaput'); });
    ipcMain.on('log', (event, msg) => __strake_send_message('main-got:' + msg));
    __strake_send_message('main-ready');
});
"#;

const RENDERER_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Renderer</title></head><body>
<div id="root"></div>
<script>
const { ipcRenderer } = require('electron');
ipcRenderer.invoke('ping', 'hello').then((reply) => {
    __strake_send_message('renderer-got:' + reply);
    document.getElementById('root').textContent = reply;
});
ipcRenderer.invoke('add', 40, 2).then((sum) => {
    __strake_send_message('renderer-sum:' + sum);
});
ipcRenderer.invoke('missing', 'x').catch((err) => {
    __strake_send_message('renderer-err:' + err.message);
});
ipcRenderer.invoke('boom').catch((err) => {
    __strake_send_message('renderer-boom:' + err.message);
});
ipcRenderer.send('log', 'fire-and-forget');
</script>
</body></html>"#;

fn harness() -> (ScriptDocument, ScriptDocument, ElectronHost) {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut main =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    main.install_electron(&host);
    main.eval(MAIN_JS);
    assert!(
        main.take_js_errors().is_empty(),
        "main.js must evaluate cleanly"
    );
    main.mark_electron_ready();
    assert_eq!(main.take_messages(), vec!["main-ready"]);

    let mut renderer = ScriptDocument::from_html(RENDERER_HTML, DocumentConfig::default())
        .without_timer_thread()
        .with_virtual_time();
    renderer.install_electron_renderer(&host);
    assert!(
        renderer.take_js_errors().is_empty(),
        "renderer bootstrap must install cleanly"
    );
    renderer.execute_scripts();
    assert!(
        renderer.take_js_errors().is_empty(),
        "renderer page must evaluate cleanly"
    );
    (main, renderer, host)
}

#[test]
fn invoke_round_trip_settles_renderer_promises() {
    let (mut main, mut renderer, host) = harness();
    // Four invokes + one send queued, nothing settled yet.
    assert_eq!(host.pending_ipc_count(), 5);

    let pumped = main.pump_ipc(&mut renderer);
    assert_eq!(pumped, 5, "every queued call pumps exactly once");
    assert_eq!(host.pending_ipc_count(), 0);

    // The intentionally-throwing `boom` handler is recorded in main (like any
    // listener throw) while its renderer promise still rejects.
    let main_errors = main.take_js_errors();
    assert_eq!(
        main_errors.len(),
        1,
        "one recorded throw, got {main_errors:?}"
    );
    assert!(
        main_errors[0].contains("kaput"),
        "unexpected main error: {}",
        main_errors[0]
    );
    assert!(
        renderer.take_js_errors().is_empty(),
        "renderer continuations must not throw"
    );
    assert!(
        renderer.take_js_errors().is_empty(),
        "renderer continuations must not throw"
    );

    assert_eq!(
        main.take_messages(),
        vec!["main-got:fire-and-forget"],
        "send fans out to ipcMain.on"
    );
    assert_eq!(
        renderer.take_messages(),
        vec![
            "renderer-got:pong:hello",
            "renderer-sum:42",
            "renderer-err:No handler registered for 'missing'",
            "renderer-boom:kaput",
        ]
    );

    // The renderer page itself observed the reply through the DOM engine.
    let guard = renderer.inner();
    let text = guard.find_body_node().expect("body").text_content();
    assert!(
        text.contains("pong:hello"),
        "reply rendered into the page DOM: {text:?}"
    );
}

#[test]
fn renderer_module_is_ipc_only() {
    let (_main, mut renderer, _host) = harness();
    renderer.eval(
        "const modr = require('electron'); \
         __strake_send_message('ipcRenderer:' + typeof modr.ipcRenderer); \
         __strake_send_message('app:' + typeof modr.app); \
         __strake_send_message('ipcMain:' + typeof modr.ipcMain); \
         __strake_send_message('BW:' + typeof modr.BrowserWindow);",
    );
    assert!(renderer.take_js_errors().is_empty());
    assert_eq!(
        renderer.take_messages(),
        vec![
            "ipcRenderer:object",
            "app:undefined",
            "ipcMain:undefined",
            "BW:undefined",
        ]
    );
}

#[test]
fn payloads_marshal_structurally() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut main =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    main.install_electron(&host);
    main.eval(
        "const { app, ipcMain } = require('electron'); \
         app.whenReady().then(() => { \
             ipcMain.handle('echo', (event, v) => v); \
             __strake_send_message('main-ready'); \
         });",
    );
    main.mark_electron_ready();

    let mut renderer = ScriptDocument::from_html(
        "<html><body><script></script></body></html>",
        DocumentConfig::default(),
    )
    .without_timer_thread()
    .with_virtual_time();
    renderer.install_electron_renderer(&host);
    renderer.execute_scripts();
    renderer.eval(
        "const { ipcRenderer } = require('electron'); \
         ipcRenderer.invoke('echo', { s: 'x', n: 1.5, b: true, z: null, a: [1, 'two', false], o: { nested: [{}] }, u: undefined, f: () => 1 }).then((v) => { \
             __strake_send_message('echo:' + JSON.stringify(v)); \
         });",
    );
    assert!(renderer.take_js_errors().is_empty());
    assert_eq!(main.pump_ipc(&mut renderer), 1);
    assert!(main.take_js_errors().is_empty());
    assert!(renderer.take_js_errors().is_empty());
    assert_eq!(
        renderer.take_messages(),
        vec![
            "echo:{\"a\":[1,\"two\",false],\"b\":true,\"n\":1.5,\"o\":{\"nested\":[{}]},\"s\":\"x\",\"z\":null}"
        ]
    );
}

#[test]
fn pump_without_host_is_a_noop() {
    let mut main =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    let mut renderer =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    assert_eq!(main.pump_ipc(&mut renderer), 0);
}
