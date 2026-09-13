//! Headed paint document for an Electron app window (issues #145, #146).
//!
//! [`paint_app_window`] builds the exact document the headed proof hands to
//! a real OS surface: entry HTML parsed with the entry file's `file://` URL
//! as its base (same as boot's first-paint path), the window's preload
//! evaluated in renderer scope, then the page's own scripts executed
//! (preload-before-scripts, the boot order), then linked stylesheets fetched,
//! ingested, and re-resolved before first paint. Local (`file:`) stylesheets
//! are render-blocking and must settle; remote (`http(s):`) font stylesheets
//! and `@font-face` files fetch async with a bounded timeout and degrade to
//! the system fallback on failure (issue #150).
//!
//! This lives here rather than in `strake-electron-compat` because script
//! execution needs [`ScriptDocument`] plus the renderer [`ElectronHost`],
//! and that crate cannot depend back on this one. It lives here rather than
//! in the `strake-run` headed binary so the whole pipeline is covered by
//! this crate's CI-runnable tests (the binary's headed path needs a display
//! and never runs in CI).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use strake_dom::{BaseDocument, DEFAULT_CSS, Document, DocumentConfig};
use strake_electron_compat::FileOnlyNetProvider;
use strake_traits::shell::{ColorScheme, Viewport};

use crate::{ElectronHost, ScriptDocument};

/// How long [`paint_app_window`] waits for linked (render-blocking)
/// resources to settle before giving up. `file:` delivery through
/// [`FileOnlyNetProvider`] is synchronous; `http(s):` delivery is async on a
/// background thread with its own 10s client timeout, so this trips on
/// genuine pipeline stalls or a stalled network, never on fast failures
/// (which drain to the fallback via the net error callback).
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a headed paint document refused to build.
#[derive(Debug)]
pub enum PaintError {
    /// The entry path could not be expressed as a `file://` base URL, so
    /// relative linked resources could never resolve.
    BaseUrl { path: PathBuf },
    /// Linked render-blocking resources were still in flight after
    /// [`SETTLE_TIMEOUT`]; first paint cannot proceed without them.
    UnsettledResources,
}

impl std::fmt::Display for PaintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BaseUrl { path } => {
                write!(f, "cannot form file:// base URL for {}", path.display())
            }
            Self::UnsettledResources => write!(
                f,
                "linked resources did not settle within {}s",
                SETTLE_TIMEOUT.as_secs()
            ),
        }
    }
}

impl std::error::Error for PaintError {}

/// A fully-loaded headed paint document plus the observations the headed
/// proof asserts on. JS errors are data, not a [`PaintError`]: callers decide
/// the policy (the headed proof fails on any; tests pin exact sets).
/// (`ScriptDocument` is not `Debug`, so neither is this.)
pub struct PaintedAppWindow {
    /// The executed, styled, first-painted document, ready for handoff to a
    /// real OS surface as `Box<dyn Document>`.
    pub document: ScriptDocument,
    /// The entry page `<title>`, if the page sets one.
    pub title: Option<String>,
    /// The painted body's text (preload/page-script effects included).
    pub body_text: String,
    /// JS errors from the preload and page scripts, drained after execution.
    pub js_errors: Vec<String>,
    /// Whether a preload source was evaluated (drives the proof's
    /// preload-effects assertion).
    pub had_preload: bool,
    /// Same-origin (`file:`) stylesheet hrefs linked from the entry page
    /// that exist on disk: every one of these must have fetched.
    pub expected_stylesheets: Vec<String>,
    /// Subresource URLs the provider actually served bytes for.
    pub served_urls: Vec<String>,
    /// Author stylesheets in the cascade after first paint.
    pub author_stylesheet_count: usize,
}

/// Stylesheet `<link>` hrefs that resolve to on-disk `file:` URLs: the
/// headed proof's "must have fetched" set. Mirrors the mutator's own
/// `rel` matching (case-sensitive `stylesheet` token) so the expectation
/// covers exactly what the pipeline attempts. Unresolvable and off-origin
/// hrefs are excluded: they keep today's skip behavior (issues #55, #146).
fn linked_file_stylesheets(doc: &BaseDocument, base_url: &str) -> Vec<String> {
    let Ok(base) = url::Url::parse(base_url) else {
        return Vec::new();
    };
    let Ok(links) = doc.query_selector_all("link") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in links {
        let Some(node) = doc.get_node(id) else {
            continue;
        };
        let is_stylesheet = node
            .attr(strake_dom::local_name!("rel"))
            .is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|token| token == "stylesheet")
            });
        if !is_stylesheet {
            continue;
        }
        let Some(href) = node.attr(strake_dom::local_name!("href")) else {
            continue;
        };
        let Ok(url) = base.join(href) else {
            continue;
        };
        if url.scheme() != "file" {
            continue;
        }
        let Ok(path) = url.to_file_path() else {
            continue;
        };
        if path.is_file() {
            out.push(url.to_string());
        }
    }
    out
}

/// Build the headed paint document for one app window.
///
/// `html` is the entry page source, `entry_path` its file (the document
/// base), `preload_source` the window's preload when the window declares one.
/// `host` is the boot main-process host that ran the app's `main` script:
/// the paint snapshots its window registry, app identity, display, and
/// capability grants (see [`ElectronHost::snapshot_for_paint`]), so a
/// preload observing main-process state paints headed exactly what the
/// headless boot observed. The live boot host is never installed into the
/// paint document and never mutated by it. Runs preload first, then page
/// scripts (the boot order, issue #145), then pumps the fetch/ingest loop
/// until linked same-origin stylesheets settle and re-resolves to first
/// paint (issue #146).
pub fn paint_app_window(
    html: &str,
    entry_path: &Path,
    host: &ElectronHost,
    width: u32,
    height: u32,
    preload_source: Option<&str>,
) -> Result<PaintedAppWindow, PaintError> {
    // Same base as boot's first-paint path: relative page resources
    // (`./styles.css`, `./renderer.js`) resolve against the entry file.
    let base_url = url::Url::from_file_path(entry_path)
        .map(|url| url.to_string())
        .map_err(|()| PaintError::BaseUrl {
            path: entry_path.to_path_buf(),
        })?;
    let net = Arc::new(FileOnlyNetProvider::new());
    let mut document = ScriptDocument::from_html(
        html,
        DocumentConfig {
            base_url: Some(base_url.clone()),
            viewport: Some(Viewport::new(width, height, 1.0, ColorScheme::Light)),
            ua_stylesheets: Some(vec![String::from(DEFAULT_CSS)]),
            net_provider: Some(Arc::clone(&net) as _),
            ..Default::default()
        },
    )
    .without_timer_thread()
    .with_virtual_time();
    // A snapshot of the boot host, installed as this document's renderer
    // host (issue #147): the preload observes the booted window registry
    // (`getAllWindows()`), while `process.versions` keeps the
    // `0.0.0-strake` runtime fallbacks the proof asserts — app identity
    // never leaks into the runtime versions. The boot host itself is
    // untouched: renderer installs overwrite the shared `renderer_module`
    // slot, so sharing would clobber the boot renderer.
    let snapshot = host.snapshot_for_paint();
    document.install_electron_renderer(&snapshot);
    // Renderer-bootstrap noise is discarded, like boot.
    document.take_js_errors();
    // Preload runs after document creation, before page scripts
    // (`execute_scripts` below): the boot order.
    if let Some(source) = preload_source {
        document.eval(source);
    }
    document.execute_scripts();
    let js_errors = document.take_js_errors();

    // Stylesheet fetch/ingest loop: each `resolve` ingests arrived bytes and
    // holds style/layout while render-blocking fetches are in flight; loop
    // until the gate clears (first paint) or the backstop trips.
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        document.inner_mut().resolve(0.0);
        if !document.inner().has_pending_critical_resources() {
            break;
        }
        if Instant::now() >= deadline {
            return Err(PaintError::UnsettledResources);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Font grace (issue #150): `@font-face` files fetch fire-and-forget
    // (non-render-blocking by design — first paint must not wait on fonts),
    // so the gate above can clear while a font is still in flight. Settle
    // briefly for quiescence so the snapshot below includes arrived fonts
    // instead of racing the worker thread. Bounded: breaks after 200ms of
    // no new deliveries (offline/unreachable degrades immediately) with a
    // 5s backstop.
    {
        let mut last_count = net.served_urls().len();
        let mut quiet_since = Instant::now();
        let grace_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            document.inner_mut().resolve(0.0);
            let count = net.served_urls().len();
            if count != last_count {
                last_count = count;
                quiet_since = Instant::now();
            }
            if Instant::now().duration_since(quiet_since) >= Duration::from_millis(200) {
                break;
            }
            if Instant::now() >= grace_deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let inner = document.inner();
    let title = inner.find_title_node().map(|node| node.text_content());
    let body_text = inner
        .find_body_node()
        .map(|node| node.text_content())
        .unwrap_or_default();
    let expected_stylesheets = linked_file_stylesheets(&inner, &base_url);
    let author_stylesheet_count = inner.author_stylesheets().count();
    drop(inner);
    Ok(PaintedAppWindow {
        document,
        title,
        body_text,
        js_errors,
        had_preload: preload_source.is_some(),
        expected_stylesheets,
        served_urls: net.served_urls(),
        author_stylesheet_count,
    })
}
