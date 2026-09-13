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

/// Floats take effect on the headed paint path (issue #149): author
/// `float: left/right` must not be silently dropped at the Stylo-to-Taffy
/// bridge. Two 150px columns inside a 300px container sit side-by-side when
/// floats apply; without `taffy/float_layout` they stack vertically.
#[test]
fn paint_applies_css_floats_side_by_side() {
    use strake_dom::Document as _;
    let entry = std::env::temp_dir().join("strake-paint-float-index.html");
    let html = r#"<!DOCTYPE html><html><head><title>Float</title></head>
        <body style="margin:0">
        <div id="wrap" style="width:300px;">
        <div id="left" style="float:left; width:150px; height:50px;"></div>
        <div id="right" style="float:right; width:150px; height:50px;"></div>
        </div>
        </body></html>"#;
    let painted = paint_app_window(html, &entry, "float-fixture", 800, 600, None)
        .expect("float fixture paints");
    assert!(
        painted.js_errors.is_empty(),
        "no scripts, no errors, got {:?}",
        painted.js_errors
    );
    let inner = painted.document.inner();
    let left_id = inner
        .query_selector("#left")
        .expect("left column query parses")
        .expect("left column exists");
    let right_id = inner
        .query_selector("#right")
        .expect("right column query parses")
        .expect("right column exists");
    let left = inner
        .get_client_bounding_rect(left_id)
        .expect("left column lays out");
    let right = inner
        .get_client_bounding_rect(right_id)
        .expect("right column lays out");
    // Side-by-side: same band (y overlap) with distinct x. Stacked (floats
    // dropped) would place right below left (y >= 50, x == 0).
    assert!(
        (left.y - right.y).abs() < 1.0,
        "float columns share a band, got left y={} right y={}",
        left.y,
        right.y
    );
    assert!(
        (right.x - left.x).abs() > 10.0,
        "float columns sit side-by-side, got left x={} right x={}",
        left.x,
        right.x
    );
}

/// Remote webfonts fetch over http(s) with offline fallback (issue #150):
/// a local HTTP fixture serves a font stylesheet plus the `@font-face` file
/// it names (hermetic, never googleapis). The headed paint must serve both
/// URLs, ingest the stylesheet, and still paint the body.
#[test]
fn paint_fetches_remote_webfonts_hermetically() {
    use std::io::{Read, Write};
    let font_bytes: Vec<u8> = strake_dom::AHEM_FONT.to_vec();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture server binds");
    let port = listener.local_addr().expect("fixture server addr").port();
    let css_url = format!("http://127.0.0.1:{port}/fonts.css");
    let font_url = format!("http://127.0.0.1:{port}/ahem.ttf");
    let font_url_for_server = font_url.clone();
    std::thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut served = 0;
        while std::time::Instant::now() < deadline && served < 10 {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // Accepted sockets inherit the listener's non-blocking
                    // mode: restore blocking so the read below waits for the
                    // request bytes instead of racing them (`accept` fires on
                    // connection establishment, before the client has sent
                    // anything — a single immediate read can hit WouldBlock
                    // and drop the connection, which fails loudly on Windows
                    // loopback and flakily elsewhere).
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                    // Read until the end of the request headers (GET has no
                    // body), tolerating segmented delivery.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    let read_deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs(5);
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                continue;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                if std::time::Instant::now() >= read_deadline {
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(5));
                                continue;
                            }
                            Err(_) => break,
                        }
                        if buf.len() > 16384 || std::time::Instant::now() >= read_deadline {
                            break;
                        }
                    }
                    let path = String::from_utf8_lossy(&buf)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_string();
                    if path.is_empty() {
                        continue;
                    }
                    let (status, content_type, body): (&str, &str, Vec<u8>) =
                        if path == "/fonts.css" {
                            let css = format!(
                                "@font-face {{ font-family: 'RemoteAhem'; \
                                 src: url('{font_url_for_server}') format('truetype'); }} \
                                 p {{ font-family: 'RemoteAhem', monospace; }}"
                            );
                            ("200 OK", "text/css", css.into_bytes())
                        } else if path == "/ahem.ttf" {
                            ("200 OK", "font/ttf", font_bytes.clone())
                        } else {
                            ("404 Not Found", "text/plain", b"nope".to_vec())
                        };
                    let header = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(&body);
                    served += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    let entry = std::env::temp_dir().join("strake-paint-remote-font-index.html");
    let html = format!(
        "<!DOCTYPE html><html><head><title>Remote Font</title>\
         <link rel=\"stylesheet\" href=\"{css_url}\"></head>\
         <body><p>remote-font-body</p></body></html>"
    );
    let painted = paint_app_window(&html, &entry, "remote-font-fixture", 800, 600, None)
        .expect("remote-font page paints");
    assert!(
        painted.js_errors.is_empty(),
        "no scripts, no errors, got {:?}",
        painted.js_errors
    );
    assert!(
        painted.served_urls.iter().any(|url| url == &css_url),
        "remote stylesheet fetched, got {:?}",
        painted.served_urls
    );
    assert!(
        painted.served_urls.iter().any(|url| url == &font_url),
        "@font-face file fetched, got {:?}",
        painted.served_urls
    );
    assert!(
        painted.author_stylesheet_count >= 1,
        "remote stylesheet ingests, got {}",
        painted.author_stylesheet_count
    );
    assert!(
        painted.body_text.contains("remote-font-body"),
        "body paints: {:?}",
        painted.body_text
    );
}

/// Unreachable remote fonts degrade to the fallback with no hang, crash, or
/// proof failure (issue #150): a refused connection drains the gate and
/// first paint proceeds with system fonts.
#[test]
fn paint_degrades_gracefully_when_remote_fonts_unreachable() {
    // Port 1 is privileged and closed on CI runners: connection refused fast.
    let css_url = "http://127.0.0.1:1/nope.css";
    let entry = std::env::temp_dir().join("strake-paint-offline-font-index.html");
    let html = format!(
        "<!DOCTYPE html><html><head><title>Offline Font</title>\
         <link rel=\"stylesheet\" href=\"{css_url}\"></head>\
         <body><p>offline-body</p></body></html>"
    );
    let painted = paint_app_window(&html, &entry, "offline-font-fixture", 800, 600, None)
        .expect("unreachable fonts must not fail the paint");
    assert!(
        painted.js_errors.is_empty(),
        "no scripts, no errors, got {:?}",
        painted.js_errors
    );
    assert!(
        !painted.served_urls.iter().any(|url| url == css_url),
        "unreachable stylesheet serves nothing, got {:?}",
        painted.served_urls
    );
    assert!(
        painted.body_text.contains("offline-body"),
        "body paints through the fallback: {:?}",
        painted.body_text
    );
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
