//! App-dir boot path (issue #110): `boot_app_dir` reads an Electron app
//! directory's `package.json`, runs its `main` script through the shim with
//! the app dir as the module root, marks the app ready, and reports every
//! created window with its entry HTML first-painted and its preload executed.

use std::path::{Path, PathBuf};

use strake_dom::DocumentConfig;
use strake_vibey_script::{ScriptDocument, boot_app_dir, boot_app_dir_with_host};

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

/// Issues #154/#155: a booted app can touch its own dir through `node:fs`
/// (the boot grants the app dir; everything else stays deny-by-default).
#[test]
fn boot_app_dir_grants_fs_access_to_app_dir() {
    let dir = std::env::temp_dir().join(format!("strake-boot-fs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("probe app dir creates");
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"fs-probe","main":"main.js"}"#,
    )
    .expect("package.json writes");
    std::fs::write(
        dir.join("main.js"),
        "const fs = require('fs'); \
         const { app, BrowserWindow } = require('electron'); \
         fs.writeFileSync(__dirname + '/note.txt', 'boot-writes'); \
         const back = fs.readFileSync(__dirname + '/note.txt', 'utf8'); \
         if (back !== 'boot-writes') throw new Error('roundtrip mismatch: ' + back); \
         new BrowserWindow({ width: 800, height: 600 });",
    )
    .expect("main.js writes");
    let report = boot_app_dir(&dir).expect("fs probe boots");
    assert!(
        report.js_errors.is_empty(),
        "fs roundtrip must not throw, got {:?}",
        report.js_errors
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("note.txt")).expect("probe file readable"),
        "boot-writes",
        "main script wrote through the app-dir grant"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #147: `boot_app_dir_with_host` returns the report plus a host
/// carrying the booted window registry, for headed paint to snapshot.
#[test]
fn boot_app_dir_with_host_carries_booted_windows() {
    let (report, host) = boot_app_dir_with_host(&fixture_app_dir()).expect("fixture app boots");
    assert_eq!(report.app_name, "mini-app");
    assert_eq!(
        host.window_count(),
        report.windows.len(),
        "host carries every booted window"
    );
    assert_eq!(host.live_window_ids(), vec![0]);
    let pending = host
        .window_pending_url(0)
        .expect("booted window has an entry target");
    assert!(
        pending.ends_with("index.html"),
        "host entry target matches report, got {pending}"
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

#[test]
fn boot_app_dir_with_ipc_proof_round_trips_through_booted_processes() {
    let (report, proof) = strake_vibey_script::boot_app_dir_with_ipc_proof(&fixture_app_dir())
        .expect("fixture app boots with proof");
    assert!(
        report.js_errors.is_empty(),
        "main.js must run cleanly, got {:?}",
        report.js_errors
    );
    assert_eq!(report.windows.len(), 1, "exactly one window");
    assert_eq!(proof.pumped, 1, "exactly one probe call pumps");
    assert_eq!(
        proof.reply.as_deref(),
        Some("strake:pong"),
        "renderer promise settles with the main reply"
    );
    assert!(
        proof.main_errors.is_empty(),
        "probe registration must not throw, got {:?}",
        proof.main_errors
    );
    assert!(
        proof.renderer_errors.is_empty(),
        "probe invocation must not throw, got {:?}",
        proof.renderer_errors
    );
    assert!(proof.succeeded());
}

/// Issue #155: apps that gate window creation behind timer-polled
/// readiness (Joplin's `waitForElectronAppReady`) still boot a window:
/// the boot path pumps virtual timers until windows appear.
#[test]
fn boot_app_dir_pumps_timer_polled_readiness() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interval-app");
    let report = boot_app_dir(&dir).expect("interval fixture app boots");
    assert!(
        report.js_errors.is_empty(),
        "main.js must run cleanly, got {:?}",
        report.js_errors
    );
    assert_eq!(report.windows.len(), 1, "timer-gated window appears");
    assert!(
        report.windows[0].entry_file.ends_with("index.html"),
        "entry HTML resolved, got {}",
        report.windows[0].entry_file
    );
}

/// The IPC proof must not depend on the app shipping a preload: apps
/// without `webPreferences` (like the calculator demo) still boot a
/// renderer for the first window so the main/renderer pair proves out.
#[test]
fn boot_app_dir_with_ipc_proof_works_without_preload() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini-app-no-preload");
    let (report, proof) = strake_vibey_script::boot_app_dir_with_ipc_proof(&dir)
        .expect("no-preload fixture app boots with proof");
    assert!(
        report.js_errors.is_empty(),
        "main.js must run cleanly, got {:?}",
        report.js_errors
    );
    assert_eq!(report.windows.len(), 1, "exactly one window");
    assert!(
        report.windows[0].preload.is_none(),
        "fixture really has no preload"
    );
    assert_eq!(proof.pumped, 1, "probe call pumps without a preload");
    assert_eq!(
        proof.reply.as_deref(),
        Some("strake:pong"),
        "renderer promise settles with the main reply"
    );
    assert!(proof.succeeded());
}

/// Portable-profile shape (issue #155): the app writes outside its own
/// dir (Joplin's `$PORTABLE_EXECUTABLE_DIR/JoplinProfile`). Without a
/// grant that records `EACCES`; `BootOptions.extra_fs_grants` allows it.
#[test]
fn boot_app_dir_with_options_grants_extra_fs_dirs() {
    let root = std::env::temp_dir().join(format!("strake-boot-grants-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("app");
    let profile = root.join("profile");
    std::fs::create_dir_all(&dir).expect("probe app dir creates");
    std::fs::create_dir_all(&profile).expect("probe profile dir creates");
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"grant-probe","main":"main.js"}"#,
    )
    .expect("package.json writes");
    std::fs::write(
        dir.join("main.js"),
        "const fs = require('fs'); \
         const path = require('path'); \
         const profile = path.join(__dirname, '..', 'profile'); \
         fs.mkdirSync(path.join(profile, 'sub'), { recursive: true }); \
         fs.writeFileSync(path.join(profile, 'note.txt'), 'profile-writes');",
    )
    .expect("main.js writes");

    let plain = boot_app_dir(&dir).expect("ungranted boot still reports");
    assert!(
        plain.js_errors.iter().any(|e| e.contains("EACCES")),
        "outside-grant writes must record EACCES, got {:?}",
        plain.js_errors
    );

    let options = strake_vibey_script::BootOptions {
        extra_fs_grants: vec![profile.clone()],
        ..Default::default()
    };
    let (report, _) =
        strake_vibey_script::boot_app_dir_with_options(&dir, &options).expect("granted boot runs");
    assert!(
        report.js_errors.is_empty(),
        "granted writes must not throw, got {:?}",
        report.js_errors
    );
    assert_eq!(
        std::fs::read_to_string(profile.join("note.txt")).expect("probe file readable"),
        "profile-writes",
        "main script wrote through the extra grant"
    );
    let _ = std::fs::remove_dir_all(&root);
}
