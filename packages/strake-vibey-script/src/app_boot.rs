//! Boot an Electron app directory headlessly (issue #110, the `npm start`
//! equivalent).
//!
//! [`boot_app_dir`] reads `<app-dir>/package.json`, runs its `main` script
//! through the [`crate::ElectronHost`] shim with the app dir as the module
//! root, marks the app ready, and reports every created window: geometry, the
//! resolved entry HTML first-painted through the DOM pipeline (with its
//! `<title>`), and the `webPreferences.preload` file executed in renderer
//! scope before page scripts.
//!
//! The windows themselves stay headless: handing a window to a real OS
//! surface is [`strake_electron_compat::ShellWindow::attach`] plus
//! `into_window_config` on a live winit event loop (the headed second half
//! of #110), which consumes the ids this report carries.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use strake_dom::{BaseDocument, DEFAULT_CSS, DocumentConfig};
use strake_html::{DocumentHtmlParser, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};

use crate::{ElectronHost, ScriptDocument};
use strake_dom::Document as _;

/// One window the app's `main` script created, with its entry page loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootedWindow {
    /// Compat window id (feeds `ShellWindow::attach` for the headed handoff).
    pub id: u32,
    /// Content width in DIP.
    pub width: u32,
    /// Content height in DIP.
    pub height: u32,
    /// Resolved entry HTML path, or the raw navigation target when it names
    /// no local file (e.g. `loadURL("https://…")`).
    pub entry_file: String,
    /// The entry page `<title>` after headless first paint, if any.
    pub page_title: Option<String>,
    /// Recorded `webPreferences.preload` path, if the window declared one.
    pub preload: Option<String>,
    /// JS errors from executing the preload, if it ran.
    pub preload_errors: Vec<String>,
}

/// What booting an app directory produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppBootReport {
    /// `package.json` `name` (falls back to the directory name).
    pub app_name: String,
    /// `package.json` `main` as written (`"main.js"` default per Electron).
    pub main_entry: String,
    /// Windows the `main` script created, in ascending id order.
    pub windows: Vec<BootedWindow>,
    /// JS errors from the `main` script itself (evaluate + ready), if any.
    pub js_errors: Vec<String>,
}

/// Why an app directory refused to boot.
#[derive(Debug)]
pub enum BootError {
    /// No `package.json` in the directory (or it could not be read).
    MissingPackageJson { dir: PathBuf },
    /// `package.json` is not valid JSON or not an object.
    InvalidPackageJson { path: PathBuf, message: String },
    /// The `main` script (or another app file) could not be read.
    UnreadableFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPackageJson { dir } => {
                write!(f, "no package.json in {}", dir.display())
            }
            Self::InvalidPackageJson { path, message } => {
                write!(f, "invalid package.json at {}: {message}", path.display())
            }
            Self::UnreadableFile { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for BootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnreadableFile { source, .. } => Some(source),
            Self::MissingPackageJson { .. } | Self::InvalidPackageJson { .. } => None,
        }
    }
}

fn read_file(path: &Path) -> Result<String, BootError> {
    std::fs::read_to_string(path).map_err(|source| BootError::UnreadableFile {
        path: path.to_path_buf(),
        source,
    })
}

/// Parse the entry HTML and resolve layout at the window size (headless first
/// paint), returning the page `<title>`, if the page sets one. `base_url` is
/// the entry file's `file://` URL so relative resources (`./styles.css`)
/// resolve instead of panicking the resolver.
fn first_paint_title(
    html: &str,
    width: u32,
    height: u32,
    base_url: Option<String>,
) -> Option<String> {
    let config = DocumentConfig {
        viewport: Some(Viewport::new(width, height, 1.0, ColorScheme::Light)),
        ua_stylesheets: Some(vec![String::from(DEFAULT_CSS)]),
        html_parser_provider: Some(Arc::new(HtmlProvider)),
        base_url,
        ..Default::default()
    };
    let mut doc = BaseDocument::new(config);
    let mut mutr = doc.mutate();
    DocumentHtmlParser::parse_into_mutator(&mut mutr, html);
    drop(mutr);
    doc.resolve(0.0);
    doc.find_title_node().map(|node| node.text_content())
}

/// Map a `loadFile`/`loadURL` target to a local path: the `file://` URL the
/// compat core records when it exists on disk, else the app dir joined with
/// the target's file name. Returns `None` for non-file targets.
fn resolve_entry_file(app_dir: &Path, target: &str) -> Option<PathBuf> {
    if let Some(path) = target.strip_prefix("file://") {
        let direct = PathBuf::from(path);
        if direct.is_file() {
            return Some(direct);
        }
        if let Some(name) = direct.file_name() {
            let under_app = app_dir.join(name);
            if under_app.is_file() {
                return Some(under_app);
            }
        }
        return None;
    }
    None
}

/// Proof that one IPC round-trip crossed the booted app's own processes
/// (issue #84): the demo app ships no IPC flow of its own, so the harness
/// registers a probe `ipcMain.handle` on the main side and invokes it from
/// the first window's renderer through [`ScriptDocument::pump_ipc`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IpcProof {
    /// Calls the pump delivered.
    pub pumped: usize,
    /// The renderer's observed reply, if the promise settled.
    pub reply: Option<String>,
    /// JS errors from the probe handler registration.
    pub main_errors: Vec<String>,
    /// JS errors from the probe invocation.
    pub renderer_errors: Vec<String>,
}

impl IpcProof {
    /// The round-trip genuinely succeeded: `strake:pong` came back clean.
    pub fn succeeded(&self) -> bool {
        self.reply.as_deref() == Some("strake:pong")
            && self.main_errors.is_empty()
            && self.renderer_errors.is_empty()
    }
}

/// Boot an Electron app directory headlessly (issue #110).
///
/// Reads `<app-dir>/package.json` (`main`, default `"index.js"`), evaluates
/// the main script with the app dir as `__dirname`, marks the app ready, and
/// loads each created window's entry HTML: first paint plus preload execution
/// in renderer scope. Remote (`http(s)`) targets are recorded, not fetched.
pub fn boot_app_dir(app_dir: &Path) -> Result<AppBootReport, BootError> {
    boot_inner(app_dir, false, &[]).map(|(report, _)| report)
}

/// Knobs for [`boot_app_dir_with_options`].
#[derive(Debug, Clone, Default)]
pub struct BootOptions {
    /// Extra filesystem roots the booted app may read and write (issue
    /// #155): portable profiles live outside the app dir, so the launcher
    /// grants them explicitly. Each entry covers its subtree, read+write.
    pub extra_fs_grants: Vec<PathBuf>,
    /// Drive one IPC round-trip through the first window's renderer, as in
    /// [`boot_app_dir_with_ipc_proof`].
    pub prove_ipc: bool,
}

/// [`boot_app_dir`], plus an [`IpcProof`] and launcher-granted filesystem
/// roots: `options.extra_fs_grants` extends the boot manifest past the app
/// dir (portable profiles), and `options.prove_ipc` adds the round-trip.
pub fn boot_app_dir_with_options(
    app_dir: &Path,
    options: &BootOptions,
) -> Result<(AppBootReport, IpcProof), BootError> {
    boot_inner(app_dir, options.prove_ipc, &options.extra_fs_grants)
}

/// [`boot_app_dir`], plus an [`IpcProof`] round-trip through the first
/// window's renderer (issue #84).
pub fn boot_app_dir_with_ipc_proof(app_dir: &Path) -> Result<(AppBootReport, IpcProof), BootError> {
    boot_inner(app_dir, true, &[])
}

/// [`boot_app_dir`], plus the boot main-process host for headed paint
/// (issue #147): a JS-clean snapshot (no context-bound registrations —
/// see [`ElectronHost::snapshot_for_paint`]), safe to hand to
/// [`crate::paint_app_window`].
pub fn boot_app_dir_with_host(app_dir: &Path) -> Result<(AppBootReport, ElectronHost), BootError> {
    let (report, _, host) = boot_inner_with_host(app_dir, false, &[])?;
    Ok((report, host))
}

fn boot_inner(
    app_dir: &Path,
    prove_ipc: bool,
    extra_fs_grants: &[PathBuf],
) -> Result<(AppBootReport, IpcProof), BootError> {
    boot_inner_with_host(app_dir, prove_ipc, extra_fs_grants)
        .map(|(report, proof, _)| (report, proof))
}

/// Timer steps the boot settle loop may consume (issue #155).
const SETTLE_TIMER_BUDGET: u32 = 64;

/// Drive virtual timers to settle async boot (issue #155). Real bundles
/// gate window creation behind timer-polled readiness (`setInterval`
/// watching `app.isReady()`), which never fires on a stopped virtual
/// clock — so jump the clock deadline-to-deadline and poll until no timers
/// remain, a window appears, or the budget is spent. Bounded because
/// recurring housekeeping intervals (update checks, log rotation) never
/// drain on their own; stopping at the first window keeps long-delay
/// callbacks (which may do real network I/O) from firing during boot.
fn settle_boot_timers(doc: &mut ScriptDocument, host: &ElectronHost) {
    for _ in 0..SETTLE_TIMER_BUDGET {
        if !host.live_window_ids().is_empty() {
            break;
        }
        let Some(deadline) = doc.next_timer_deadline() else {
            break;
        };
        doc.advance_clock_to(deadline);
        doc.poll(None);
    }
}

fn boot_inner_with_host(
    app_dir: &Path,
    prove_ipc: bool,
    extra_fs_grants: &[PathBuf],
) -> Result<(AppBootReport, IpcProof, ElectronHost), BootError> {
    let manifest_path = app_dir.join("package.json");
    if !manifest_path.is_file() {
        return Err(BootError::MissingPackageJson {
            dir: app_dir.to_path_buf(),
        });
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&read_file(&manifest_path)?).map_err(|error| {
            BootError::InvalidPackageJson {
                path: manifest_path.clone(),
                message: error.to_string(),
            }
        })?;
    let manifest = manifest
        .as_object()
        .ok_or_else(|| BootError::InvalidPackageJson {
            path: manifest_path.clone(),
            message: String::from("top level must be an object"),
        })?;
    let app_name = manifest
        .get("name")
        .and_then(|name| name.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            app_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| String::from("app"))
        });
    let app_version = manifest
        .get("version")
        .and_then(|version| version.as_str())
        .unwrap_or("0.0.0");
    let main_entry = manifest
        .get("main")
        .and_then(|main| main.as_str())
        .unwrap_or("index.js");
    let main_path = app_dir.join(main_entry);
    let main_source = read_file(&main_path)?;

    // An app can always touch its own dir through `node:fs` (issues
    // #154/#155): the boot grants the canonicalized app dir read+write,
    // everything else stays deny-by-default. Broader grants (userData,
    // home, removable media) ride with the #16 permission UX, not the
    // boot path.
    let grant_root = std::fs::canonicalize(app_dir).unwrap_or_else(|_| app_dir.to_path_buf());
    let grant_scope = format!("{}/*", grant_root.to_string_lossy().replace('\\', "/"));
    // Launcher-granted roots past the app dir (portable profiles, issue
    // #155): canonicalized like the app dir so symlinked temp dirs match
    // the canonical access spelling the fs gate checks.
    let mut fs_read = vec![strake_electron_compat::PathScope::new(&grant_scope)];
    let mut fs_write = vec![strake_electron_compat::PathScope::new(&grant_scope)];
    for extra in extra_fs_grants {
        let root = std::fs::canonicalize(extra).unwrap_or_else(|_| extra.clone());
        let scope = format!("{}/*", root.to_string_lossy().replace('\\', "/"));
        fs_read.push(strake_electron_compat::PathScope::new(&scope));
        fs_write.push(strake_electron_compat::PathScope::new(&scope));
    }
    let host = ElectronHost::new(&app_name, app_version).with_permissions(
        strake_electron_compat::PermissionManifest {
            fs_read,
            fs_write,
            ..Default::default()
        },
    );
    let mut doc =
        ScriptDocument::from_html("<html><body></body></html>", DocumentConfig::default())
            .without_timer_thread()
            .with_virtual_time();
    doc.install_electron(&host);
    let mut js_errors = doc.take_js_errors();
    if let Some(root) = app_dir.to_str() {
        doc.set_node_app_root(root);
    }
    doc.eval(&main_source);
    js_errors.extend(doc.take_js_errors());
    doc.mark_electron_ready();
    js_errors.extend(doc.take_js_errors());
    settle_boot_timers(&mut doc, &host);
    js_errors.extend(doc.take_js_errors());

    let mut windows = Vec::new();
    let mut ipc_proof = IpcProof::default();
    let mut ipc_proven = false;
    for id in host.live_window_ids() {
        let (width, height) = host
            .window_bounds(id)
            .map(|bounds| (bounds.width, bounds.height))
            .unwrap_or((800, 600));
        let target = host.window_pending_url(id).unwrap_or_default();
        let preload = host.window_preload(id);
        let mut window = BootedWindow {
            id,
            width,
            height,
            entry_file: target.clone(),
            page_title: None,
            preload: preload.clone(),
            preload_errors: Vec::new(),
        };
        if let Some(entry_path) = resolve_entry_file(app_dir, &target) {
            window.entry_file = entry_path.to_string_lossy().into_owned();
            // The entry file's `file://` URL is the base for relative page
            // resources (`./styles.css`, `./renderer.js`).
            let base_url = url::Url::from_file_path(&entry_path)
                .ok()
                .map(|url| url.into());
            if let Ok(html) = std::fs::read_to_string(&entry_path) {
                window.page_title = first_paint_title(&html, width, height, base_url.clone());
                // The main script joins `__dirname`, which the boot sets to
                // the app dir; still, resolve relative preload paths against
                // the app dir defensively.
                let preload_path = preload.as_deref().map(Path::new).and_then(|raw| {
                    if raw.is_file() {
                        return Some(raw.to_path_buf());
                    }
                    raw.file_name()
                        .map(|name| app_dir.join(name))
                        .filter(|path| path.is_file())
                });
                // The IPC proof needs a renderer even when the app ships no
                // preload (issue #84 canaries without `webPreferences`):
                // without one the first window can never prove its
                // main/renderer pair.
                if preload_path.is_some() || (prove_ipc && !ipc_proven) {
                    let renderer_config = DocumentConfig {
                        base_url: base_url.clone(),
                        ..Default::default()
                    };
                    let mut renderer = ScriptDocument::from_html(&html, renderer_config)
                        .without_timer_thread()
                        .with_virtual_time();
                    renderer.install_electron_renderer(&host);
                    renderer.take_js_errors();
                    if let Some(preload_path) = preload_path {
                        match std::fs::read_to_string(&preload_path) {
                            Ok(source) => {
                                // Preload runs after document creation, before
                                // page scripts (`execute_scripts` below).
                                renderer.eval(&source);
                                renderer.execute_scripts();
                                window.preload_errors = renderer.take_js_errors();
                            }
                            Err(source) => {
                                window.preload_errors.push(format!(
                                    "cannot read {}: {source}",
                                    preload_path.display()
                                ));
                            }
                        }
                    }
                    // Issue #84: one IPC round-trip through the booted app's
                    // own main/renderer pair.
                    if prove_ipc && !ipc_proven {
                        ipc_proven = true;
                        ipc_proof = prove_ipc_roundtrip(&mut doc, &mut renderer);
                    }
                }
            }
        }
        windows.push(window);
    }

    // The live host's JS-bound registrations die with `doc` below; hand out
    // a JS-clean snapshot (issue #147) so headed paint can observe the
    // booted window registry without touching dead contexts.
    let paint_host = host.snapshot_for_paint();
    Ok((
        AppBootReport {
            app_name,
            main_entry: main_entry.to_string(),
            windows,
            js_errors,
        },
        ipc_proof,
        paint_host,
    ))
}

/// Register a probe `ipcMain.handle` on the booted main side, invoke it from
/// the window's renderer, and pump: the issue #84 round-trip. The demo app
/// itself ships no IPC flow, so both endpoints are harness-driven — but the
/// handler, the transport, and the promise settlement are the app's own
/// booted processes.
fn prove_ipc_roundtrip(main: &mut ScriptDocument, renderer: &mut ScriptDocument) -> IpcProof {
    main.eval("require('electron').ipcMain.handle('strake:ping', () => 'strake:pong');");
    let main_errors = main.take_js_errors();
    renderer.eval(
        "require('electron').ipcRenderer.invoke('strake:ping').then((reply) => { \
             __strake_send_message('strake:ipc-reply:' + reply); \
         });",
    );
    let renderer_errors = renderer.take_js_errors();
    let pumped = main.pump_ipc(renderer);
    let mut reply = None;
    for message in renderer.take_messages() {
        if let Some(value) = message.strip_prefix("strake:ipc-reply:") {
            reply = Some(value.to_string());
        }
    }
    IpcProof {
        pumped,
        reply,
        main_errors,
        renderer_errors,
    }
}
