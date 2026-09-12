//! Hermetic performance-budget harness (issue #11).
//!
//! Measures, with std-only timing:
//! - cold-boot latency: parse + `resolve(0.0)` of a 1000-node fixture,
//!   50 iterations, reporting median / p95 / stddev;
//! - idle RSS after resolve, via `/proc/self/statm` (Linux only;
//!   prints SKIP elsewhere so macOS/Windows dev machines stay usable).
//!
//! Budgets (issue #11 defaults, env-overridable):
//! - `PERF_MAX_P95_MS` (default 100.0): cold-boot p95 ceiling.
//! - `PERF_MAX_RSS_MB` (default 30.0): post-resolve idle RSS ceiling.
//!
//! Exit code is 1 on any breach, naming the breached budget with
//! measured-vs-allowed values. Binary size is gated in CI (Task 3),
//! not here, because a binary cannot portably stat its own file.

use std::sync::Arc;
use std::time::Instant;

use strake_dom::DocumentConfig;
use strake_html::{HtmlDocument, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};

const ITERATIONS: usize = 50;

fn fixture() -> String {
    let mut html = String::from(
        "<!DOCTYPE html><html><head><style>\
         body { margin: 0; font-family: Ahem; font-size: 16px; }\
         .row { display: flex; width: 800px; }\
         .cell { width: 100px; height: 20px; color: rgb(10, 20, 30); }\
         </style></head><body>",
    );
    for i in 0..500 {
        html.push_str(&format!(
            "<div class=\"row\" id=\"r{i}\"><div class=\"cell\">cell {i} a</div>\
             <div class=\"cell\">cell {i} b</div></div>"
        ));
    }
    html.push_str("</body></html>");
    html
}

fn cold_boot_once(html: &str) -> f64 {
    let start = Instant::now();
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            font_ctx: Some(strake_dom::hermetic_test_font_context()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    drop(doc);
    start.elapsed().as_secs_f64() * 1000.0
}

/// Post-resolve idle RSS in MiB. `None` on non-Linux (caller reports SKIP).
fn idle_rss_mib(html: &str) -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let mut doc = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                html_parser_provider: Some(Arc::new(HtmlProvider) as _),
                font_ctx: Some(strake_dom::hermetic_test_font_context()),
                ..Default::default()
            },
        );
        doc.resolve(0.0);
        // Quiescence: resolve twice so deferred/microtask-driven work settles.
        doc.resolve(0.0);
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: f64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        drop(doc);
        // RSS pages are 4 KiB on every supported Linux target (x86_64/aarch64).
        Some(resident_pages * 4096.0 / 1024.0 / 1024.0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = html;
        None
    }
}

fn percentile(sorted: &mut [f64], p: f64) -> f64 {
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx]
}

fn env_or(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let max_p95 = env_or("PERF_MAX_P95_MS", 100.0);
    let max_rss = env_or("PERF_MAX_RSS_MB", 30.0);
    let html = fixture();

    // Warm up allocator/page cache once, outside the measured window.
    cold_boot_once(&html);

    let mut samples: Vec<f64> = (0..ITERATIONS).map(|_| cold_boot_once(&html)).collect();
    let median = percentile(&mut samples, 50.0);
    let p95 = percentile(&mut samples, 95.0);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let variance = samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / samples.len() as f64;
    let rss = idle_rss_mib(&html);

    println!("cold-boot over {ITERATIONS} iterations (ms):");
    println!(
        "  median {median:.2}  p95 {p95:.2}  mean {mean:.2}  stddev {:.2}",
        variance.sqrt()
    );
    match rss {
        Some(mib) => println!("idle RSS after resolve: {mib:.2} MiB"),
        None => println!("idle RSS: SKIP (RSS sampling is Linux-only)"),
    }

    let mut breaches = Vec::new();
    if p95 > max_p95 {
        breaches.push(format!(
            "cold-boot p95 {p95:.2} ms exceeds budget {max_p95:.2} ms"
        ));
    }
    if let Some(mib) = rss
        && mib > max_rss
    {
        breaches.push(format!(
            "idle RSS {mib:.2} MiB exceeds budget {max_rss:.2} MiB"
        ));
    }
    if !breaches.is_empty() {
        for b in &breaches {
            eprintln!("PERF BREACH: {b}");
        }
        std::process::exit(1);
    }
    println!(
        "PERF OK: p95 {p95:.2} ms (budget {max_p95:.2}), rss {} (budget {max_rss:.2} MiB)",
        rss.map(|m| format!("{m:.2} MiB"))
            .unwrap_or_else(|| "SKIP".to_string())
    );

    // Appends rows to the CI step summary. The Binary Size step (Task 3)
    // writes the table header first; use append-only here so rows from
    // both steps land in one table. Locally GITHUB_STEP_SUMMARY is unset
    // and this block is skipped.
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        use std::fmt::Write as _;
        use std::fs::OpenOptions;
        use std::io::Write as _;
        let mut table = String::new();
        let _ = writeln!(
            table,
            "| **Cold Boot (p95)** | {p95:.2} ms | {max_p95:.2} ms | ✅ PASS |"
        );
        match rss {
            Some(mib) => {
                let _ = writeln!(
                    table,
                    "| **Idle Memory (RSS)** | {mib:.2} MiB | {max_rss:.2} MiB | ✅ PASS |"
                );
            }
            None => {
                table.push_str("| **Idle Memory (RSS)** | SKIP (non-Linux) | — | ⚪ SKIP |\n");
            }
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(summary)
            .and_then(|mut f| f.write_all(table.as_bytes()))
            .expect("append step summary");
    }
}
