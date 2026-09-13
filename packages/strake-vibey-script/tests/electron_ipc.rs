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
/// Issue #92: notify → recorded delivery → click dispatch → `onclick` fires.
#[test]
fn notification_click_fires_onclick() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut renderer = ScriptDocument::from_html(
        "<html><body><script></script></body></html>",
        DocumentConfig::default(),
    )
    .without_timer_thread()
    .with_virtual_time();
    renderer.install_electron_renderer(&host);
    renderer.execute_scripts();
    renderer.eval(
        "const n = new Notification('Build done', { body: 'ok', icon: 'icon.png' }); \
         n.onclick = (e) => { __strake_send_message('clicked:' + e.type); };",
    );
    assert!(
        renderer.take_js_errors().is_empty(),
        "notification construction must not throw"
    );

    let delivered = host.notification_delivered();
    assert_eq!(delivered.len(), 1, "one recorded delivery");
    assert_eq!(delivered[0].request.title, "Build done");
    assert_eq!(delivered[0].request.body.as_deref(), Some("ok"));
    assert_eq!(
        delivered[0].request.icon.as_deref(),
        Some("icon.png"),
        "icon crosses the JS boundary via the third native arg"
    );
    assert!(!delivered[0].clicked);

    assert!(
        renderer.dispatch_notification_click(delivered[0].id),
        "known id dispatches"
    );
    assert!(
        !renderer.dispatch_notification_click(999),
        "unknown id dispatches nothing"
    );
    assert!(renderer.take_js_errors().is_empty());
    assert_eq!(renderer.take_messages(), vec!["clicked:click"]);
    assert!(
        host.notification_delivered()[0].clicked,
        "delivery marked clicked"
    );
}

/// Issue #92 review: `dispatch_notification_click` must drain the job queue
/// after invoking JS (mirroring `dispatch_power_events`), so promise
/// continuations scheduled by an `onclick` run synchronously inside dispatch
/// instead of lingering until an unrelated later pump.
#[test]
fn notification_click_flushes_async_continuations() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut renderer = ScriptDocument::from_html(
        "<html><body><script></script></body></html>",
        DocumentConfig::default(),
    )
    .without_timer_thread()
    .with_virtual_time();
    renderer.install_electron_renderer(&host);
    renderer.execute_scripts();
    renderer.eval(
        "const n = new Notification('Build done', { body: 'ok', icon: 'icon.png' }); \
         n.onclick = (e) => { Promise.resolve().then(() => { __strake_send_message('async:' + e.type); }); };",
    );
    assert!(renderer.take_js_errors().is_empty());

    let delivered = host.notification_delivered();
    assert_eq!(delivered.len(), 1, "one recorded delivery");
    assert!(
        renderer.dispatch_notification_click(delivered[0].id),
        "known id dispatches"
    );
    assert!(renderer.take_js_errors().is_empty());
    assert_eq!(
        renderer.take_messages(),
        vec!["async:click"],
        "onclick microtask continuations flush inside dispatch"
    );
}

/// Issue #92 review: the `onclick` setter follows the WebIDL EventHandler
/// conversion — a non-callable assignment normalizes to `null` without
/// throwing, and the getter and the native handler map stay in agreement
/// (dispatching afterwards fires nothing).
#[test]
fn notification_onclick_non_callable_normalizes_to_null() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut renderer = ScriptDocument::from_html(
        "<html><body><script></script></body></html>",
        DocumentConfig::default(),
    )
    .without_timer_thread()
    .with_virtual_time();
    renderer.install_electron_renderer(&host);
    renderer.execute_scripts();
    renderer.eval(
        "const n = new Notification('Build done', { body: 'ok' }); \
         n.onclick = (e) => { __strake_send_message('clicked:' + e.type); }; \
         n.onclick = 42; \
         __strake_send_message('onclick-is:' + String(n.onclick));",
    );
    assert!(
        renderer.take_js_errors().is_empty(),
        "non-callable onclick must normalize to null without throwing"
    );
    assert_eq!(renderer.take_messages(), vec!["onclick-is:null"]);

    let delivered = host.notification_delivered();
    assert_eq!(delivered.len(), 1, "one recorded delivery");
    assert!(
        renderer.dispatch_notification_click(delivered[0].id),
        "known id dispatches"
    );
    assert!(renderer.take_js_errors().is_empty());
    assert!(
        renderer.take_messages().is_empty(),
        "cleared handler fires nothing on click"
    );
    assert!(
        host.notification_delivered()[0].clicked,
        "delivery still marked clicked"
    );
}

/// Issue #91: main-process `win.webContents.send(channel, ...args)` queues
/// per target window; the pump delivers each payload to that window's
/// renderer `ipcRenderer.on` listeners as `(event, ...args)`.
#[test]
fn web_contents_send_reaches_renderer_listeners() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut main =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    main.install_electron(&host);
    main.eval(
        "const { app, BrowserWindow } = require('electron'); \
         app.whenReady().then(() => { \
             const win = new BrowserWindow({ show: false }); \
             win.webContents.send('tick', { n: 1 }, 'two'); \
             __strake_send_message('main-sent'); \
         });",
    );
    assert!(main.take_js_errors().is_empty());
    main.mark_electron_ready();
    assert_eq!(main.take_messages(), vec!["main-sent"]);
    assert_eq!(host.pending_main_send_count(), 1);

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
         ipcRenderer.on('tick', (event, payload, word) => { \
             __strake_send_message('renderer-tick:' + JSON.stringify(payload) + ':' + word + ':' + typeof event); \
         });",
    );
    assert!(renderer.take_js_errors().is_empty());

    assert_eq!(main.pump_ipc(&mut renderer), 1, "one main-send pumps once");
    assert_eq!(host.pending_main_send_count(), 0);
    assert!(main.take_js_errors().is_empty());
    assert!(renderer.take_js_errors().is_empty());
    assert_eq!(
        renderer.take_messages(),
        vec!["renderer-tick:{\"n\":1}:two:object"]
    );
}

/// Issue #91 routing note, pinned: the pump discards the target window id
/// and fans every window's outbox into the single attached renderer's channel
/// listeners, so a send addressed to window B still fires those listeners.
/// Per-window renderer binding rides with the multi-window transport (issue
/// #15); that change must visibly alter this test instead of silently
/// changing delivery.
#[test]
fn web_contents_send_fans_out_to_single_attached_renderer() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut main =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    main.install_electron(&host);
    main.eval(
        "const BW = require('electron').BrowserWindow; \
         const winA = new BW({ show: false }); \
         const winB = new BW({ show: false }); \
         winA.webContents.send('tick', 'for-A'); \
         winB.webContents.send('tick', 'for-B'); \
         __strake_send_message('main-sent');",
    );
    assert!(main.take_js_errors().is_empty());
    assert_eq!(main.take_messages(), vec!["main-sent"]);
    assert_eq!(host.pending_main_send_count(), 2);

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
         ipcRenderer.on('tick', (event, word) => { \
             __strake_send_message('renderer-tick:' + word); \
         });",
    );
    assert!(renderer.take_js_errors().is_empty());

    assert_eq!(main.pump_ipc(&mut renderer), 2);
    assert_eq!(host.pending_main_send_count(), 0);
    assert!(main.take_js_errors().is_empty());
    assert!(renderer.take_js_errors().is_empty());
    // Both windows' payloads reach the one attached renderer in window-id
    // order, including the send addressed to window B.
    assert_eq!(
        renderer.take_messages(),
        vec!["renderer-tick:for-A", "renderer-tick:for-B"]
    );
}

/// Issue #91: sends to unknown/closed windows fail softly (no throw),
/// matching Electron's fire-and-forget posture.
#[test]
fn web_contents_send_to_closed_window_fails_softly() {
    let host = ElectronHost::new("QuickStart", "1.0.0");
    let mut main =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    main.install_electron(&host);
    main.eval(
        "const BW = require('electron').BrowserWindow; \
         const doomed = new BW({ show: false }); \
         doomed.close(); \
         doomed.webContents.send('ghost', 1); \
         __strake_send_message('survived');",
    );
    assert!(
        main.take_js_errors().is_empty(),
        "send on a closed window must not throw"
    );
    assert_eq!(main.take_messages(), vec!["survived"]);
    assert_eq!(host.pending_main_send_count(), 0);

    let mut renderer =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    assert_eq!(main.pump_ipc(&mut renderer), 0);
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
