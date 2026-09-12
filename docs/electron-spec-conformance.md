# Electron spec conformance (issue #74)

Increment 1: inventory of Electron's `spec/` against the frozen shim surface
(`TOP50` in `packages/strake-electron-compat/src/coverage.rs`), the harness
that boots canary apps, and the first ported batch (lifecycle + windows +
IPC; OS bridges such as clipboard/safe-storage/power/`Notification` follow
with the #12-dependent implementations). Later increments port `dialog`/
`shell`/`nativeTheme` once #12 lands.

## Harness

- Unit level: `cargo test -p strake-electron-compat` (state machines,
  soft-fail contracts, backend recorders).
- Shim level: `packages/strake-vibey-script/tests/electron.rs` (main-process
  `main.js` shapes) and `tests/electron_ipc.rs` (renderer round-trips via
  `pump_ipc`, the in-process analogue of the cross-process transport).
- Canary level: `calculator_demo_boots_and_quits` boots the calculator
  demo's `main.js` call sequence
  (`gregoreesmaa/strake-electron-calculator@strake-demo`) with only Node
  core (`path`/`url`/`process`, owned by #16) stubbed in-test.
- Gate: all of the above run in CI (`cargo test`) as a blocking gate; WPT
  reftests ride the separate WPT workflow.

## Inventory (ported first, in shim order)

| Electron spec area | Shim surface | Covering tests | Status |
|---|---|---|---|
| `api-app-spec` lifecycle (`ready`, `window-all-closed`, `activate`, `quit`, `getName`/`getVersion`/`getPath`) | `App`, `app.on/whenReady` | `electron.rs`: quickstart, ready idempotency, quit flow | Ported |
| `api-browser-window-spec` construction, defaults (800x600, show, resizable), show/hide, bounds, `setTitle`, `closed` | `WindowManager`, `BrowserWindowOptions`, `win.on` | `electron.rs`: geometry parity, canary boot, destroyed-window throws; compat `window_state_transitions`, `closed_listeners_*` | Ported |
| `api-ipc-main/ipc-renderer-spec` `handle`/`invoke`, `send`/`on`, `webContents.send` | `IpcBus`, `WebContents` outbox, `pump_ipc` | `electron_ipc.rs`: full round-trip suite incl. main→renderer and soft-fail | Ported |
| `api-clipboard-spec` `readText`/`writeText`/`clear` | Deferred: bind strake-shell native clipboard | — | Gap: spec + shim land with the OS-bridge implementation |
| `api-safe-storage-spec` | Deferred | — | Gap: spec + shim land with the OS-bridge implementation |
| `api-power-monitor-spec`, `powerSaveBlocker` | Deferred | — | Gap: spec + shim land with the OS-bridge implementation |
| `api-screen-spec` | `Screen`/`Display` + shim | `window_geometry_*` shim test; compat/placement units | Ported |
| Web `Notification` | Deferred: renderer binding | — | Gap: spec + binding land with the OS-bridge implementation |
| `api-dialog`, `shell`, `nativeTheme`, `Menu`/`Tray`, `globalShortcut` | Deferred (needs #12 / OS bridges) | — | Gap: spec ports land with the implementations |
| `webContents` (`executeJavaScript`, `openDevTools`, `print`) | Deferred (renderer binding, devtools UI, #12 printing) | — | Gap |

## Rule (acceptance: no shim API without a spec)

Every `Shimmed` TOP50 entry names its covering test next to the mapping
above; promoting a `Deferred` entry means porting its spec cases in the same
PR, not editing prose. The freeze test (`coverage_freeze_*`) keeps the table
sorted, unique, and exactly counted so additions stay reviewed.
