//! App-dir boot path (issue #110): `boot_app_dir` reads an Electron app
//! directory's `package.json`, runs its `main` script through the shim with
//! the app dir as the module root, marks the app ready, and reports every
//! created window with its entry HTML first-painted and its preload executed.

use std::path::{Path, PathBuf};

use strake_dom::DocumentConfig;
use strake_vibey_script::{ScriptDocument, boot_app_dir};

fn fixture_app_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini-app")
}

#[test]
fn boot_app_dir_reports_window_first_paint_and_preload() {
    let report = boot_app_dir(&fixture_app_dir()).expect("fixture app boots");
    assert!(
        report.js_errors.is_empty(),
        "main.js must run cleanly, got {:?}",
        report.js_errors
    );
    assert_eq!(report.app_name, "mini-app");
    assert_eq!(report.main_entry, "main.js");
    assert_eq!(report.windows.len(), 1, "exactly one window");

    let win = &report.windows[0];
    assert_eq!((win.width, win.height), (800, 600));
    assert!(
        win.entry_file.ends_with("index.html"),
        "entry HTML resolved, got {}",
        win.entry_file
    );
    assert_eq!(
        win.page_title.as_deref(),
        Some("Hello World!"),
        "entry HTML first-painted with title"
    );
    let preload = win.preload.as_deref().expect("preload recorded");
    assert!(
        preload.ends_with("preload.js"),
        "preload path recorded, got {preload}"
    );
    assert!(
        win.preload_errors.is_empty(),
        "preload must execute cleanly, got {:?}",
        win.preload_errors
    );
}

#[test]
fn boot_app_dir_rejects_dirs_without_package_json() {
    let missing = std::env::temp_dir().join("strake-boot-test-no-such-app");
    let _ = std::fs::remove_dir_all(&missing);
    let error = boot_app_dir(&missing).unwrap_err().to_string();
    assert!(
        error.contains("package.json"),
        "missing dir names package.json, got {error}"
    );
}

#[test]
fn node_app_root_overrides_dirname() {
    // The runner points module resolution at the app dir (issue #110):
    // `__dirname` and `process.cwd()` follow the explicit root, and
    // `path.join(__dirname, ...)` resolves under it.
    let host = strake_vibey_script::ElectronHost::new("QuickStart", "1.0.0");
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    doc.set_node_app_root("/app/demo");
    doc.eval(
        "const path = require('node:path'); \
         __strake_send_message('dir:' + __dirname); \
         __strake_send_message('cwd:' + process.cwd()); \
         __strake_send_message('joined:' + path.join(__dirname, 'preload.js'));",
    );
    assert!(
        doc.take_js_errors().is_empty(),
        "app-root probes must not throw"
    );
    assert_eq!(
        doc.take_messages(),
        vec![
            "dir:/app/demo",
            "cwd:/app/demo",
            "joined:/app/demo/preload.js",
        ]
    );
}
