//! Headed paint pipeline without a display (issues #145, #146).
//!
//! [`strake_vibey_script::paint_app_window`] builds the exact document the
//! headed proof hands to a real OS surface. These tests pin the whole
//! pipeline headlessly so CI covers it: preload evaluated before page
//! scripts with DOM effects in the painted body (#145), and the linked
//! same-origin stylesheet fetched, ingested, and in the cascade (#146).

use std::path::{Path, PathBuf};
use strake_vibey_script::paint_app_window;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/headed-paint-app")
}

#[test]
fn paint_runs_preload_then_page_scripts_and_applies_linked_css() {
    let dir = fixture_dir();
    let html = std::fs::read_to_string(dir.join("index.html")).expect("fixture html");
    let preload = std::fs::read_to_string(dir.join("preload.js")).expect("fixture preload");
    let painted = paint_app_window(
        &html,
        &dir.join("index.html"),
        "paint-fixture",
        800,
        600,
        Some(&preload),
    )
    .expect("fixture app paints");
    assert!(
        painted.js_errors.is_empty(),
        "preload + page scripts run cleanly, got {:?}",
        painted.js_errors
    );
    assert!(painted.had_preload);
    assert_eq!(painted.title.as_deref(), Some("Paint Fixture"));
    // Issue #145: preload DOM effects (DOMContentLoaded version stamping)
    // and page-script effects reach the painted body.
    assert!(
        painted.body_text.contains("0.0.0-strake"),
        "preload version stamps in painted body: {:?}",
        painted.body_text
    );
    assert!(
        painted.body_text.contains("page-script-ran"),
        "inline page script ran before first paint: {:?}",
        painted.body_text
    );
    // Issue #146: the one linked same-origin stylesheet fetched and joined
    // the cascade before first paint.
    assert_eq!(
        painted.expected_stylesheets.len(),
        1,
        "exactly the linked styles.css is expected, got {:?}",
        painted.expected_stylesheets
    );
    assert_eq!(
        painted.served_urls, painted.expected_stylesheets,
        "every linked same-origin stylesheet fetched",
    );
    assert_eq!(
        painted.author_stylesheet_count, 1,
        "fetched stylesheet ingests into the cascade",
    );
}

/// No preload and no linked CSS: the pipeline still paints, reporting empty
/// expectation sets so the headed proof skips both assertions honestly.
#[test]
fn paint_without_preload_or_css_reports_empty_sets() {
    // The entry file need not exist (no links to resolve), but the path must
    // be formable as a `file://` URL on every platform (`/tmp/...` is not
    // valid on Windows, which caught this in CI).
    let entry = std::env::temp_dir().join("strake-paint-bare-index.html");
    let painted = paint_app_window(
        "<!DOCTYPE html><html><head><title>Bare</title></head>\
         <body><p>bare</p></body></html>",
        &entry,
        "bare",
        800,
        600,
        None,
    )
    .expect("bare page paints");
    assert!(
        painted.js_errors.is_empty(),
        "no scripts, no errors, got {:?}",
        painted.js_errors
    );
    assert!(!painted.had_preload);
    assert_eq!(painted.title.as_deref(), Some("Bare"));
    assert!(
        painted.body_text.contains("bare"),
        "body paints: {:?}",
        painted.body_text
    );
    assert!(painted.expected_stylesheets.is_empty());
    assert!(painted.served_urls.is_empty());
    assert_eq!(painted.author_stylesheet_count, 0);
}

/// Preload JS errors are reported as data (the headed proof fails on them),
/// not as a build error: the document still reaches first paint.
#[test]
fn paint_reports_preload_errors_as_data() {
    let dir = fixture_dir();
    let html = std::fs::read_to_string(dir.join("index.html")).expect("fixture html");
    let painted = paint_app_window(
        &html,
        &dir.join("index.html"),
        "paint-fixture",
        800,
        600,
        Some("throw new Error('broken-preload');"),
    )
    .expect("throwing preload still paints");
    assert!(
        painted
            .js_errors
            .iter()
            .any(|error| error.contains("broken-preload")),
        "preload error surfaces, got {:?}",
        painted.js_errors
    );
    assert!(painted.had_preload);
}
