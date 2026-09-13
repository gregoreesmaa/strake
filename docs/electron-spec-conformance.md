# Electron spec conformance (issue #74)

Increment 1: inventory of Electron's `spec/` against the frozen shim surface
(`TOP50` in `packages/strake-electron-compat/src/coverage.rs`), the harness
that boots canary apps, and the first ported batch (lifecycle + windows +
IPC; OS bridges (clipboard/safe-storage/power/`Notification`, #98) are
ported. Later increments port `dialog`/`shell`/`nativeTheme` once #12
lands.

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
| `api-clipboard-spec` `readText`/`writeText`/`clear` | `Clipboard`/`MemoryClipboard` + shim | `electron.rs`: `os_bridges_clipboard_safe_storage_power`; compat `memory_clipboard_round_trips_and_clears` | Ported |
| `api-safe-storage-spec` | `SafeStorage` + `RecordingKeychain` + shim | `electron.rs`: `os_bridges_*`; compat `recording_backend_round_trips_strings`, `garbage_ciphertext_fails_to_decrypt`, `base64_codec_vectors` | Ported |
| `api-power-monitor-spec`, `powerSaveBlocker` | `PowerHub`/`PowerMonitor`/`PowerSaveBlocker` + shim | `electron.rs`: `os_bridges_*` (inject + dispatch); compat `synthetic_sleep_resume_reaches_listeners`, `throwing_listener_is_isolated`, `blocker_tracks_lifetimes` | Ported |
| `api-screen-spec` | `Screen`/`Display` + shim | `window_geometry_*` shim test; compat/placement units | Ported |
| Web `Notification` | `NotificationCenter` + shim (`show`/`onclick`) | `electron_ipc.rs`: `notification_click_fires_onclick`, `notification_click_flushes_async_continuations`; compat `notify_records_delivery_then_click_dispatch`, `deliveries_keep_fifo_order` | Ported |
| `api-dialog`, `shell`, `nativeTheme`, `Menu`/`Tray`, `globalShortcut` | Deferred (needs #12 / OS bridges) | — | Gap: spec ports land with the implementations |
| `webContents` (`executeJavaScript`, `openDevTools`, `print`) | Deferred (renderer binding, devtools UI, #12 printing) | — | Gap |

## Rule (acceptance: no shim API without a spec)

Every `Shimmed` TOP50 entry names its covering test next to the mapping
above; promoting a `Deferred` entry means porting its spec cases in the same
PR, not editing prose. The freeze test (`coverage_freeze_*`) keeps the table
sorted, unique, and exactly counted so additions stay reviewed.
