//! `strake-run`: boot an Electron app directory headlessly (issue #110).
//!
//! The `npm start` equivalent for Strake's drop-in layer: reads the app
//! dir's `package.json`, runs its `main` script through the Electron shim,
//! marks the app ready, first-paints each created window's entry HTML, and
//! executes preload scripts. Headless by design (no display, no event loop):
//! handing a window to a real OS surface is the headed follow-up, which
//! consumes the reported window ids via `ShellWindow::attach`.
//!
//! Exit status is 0 only when the boot, the main script, and every preload
//! ran without errors; the full report prints either way.

use std::path::PathBuf;

use strake_vibey_script::boot_app_dir;

fn usage() -> ! {
    eprintln!("usage: strake-run <app-dir>");
    std::process::exit(2);
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| usage());
    let report = match boot_app_dir(&PathBuf::from(&dir)) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("strake-run: {error}");
            std::process::exit(1);
        }
    };

    println!("app: {} (main: {})", report.app_name, report.main_entry);
    if report.js_errors.is_empty() {
        println!("main: ok, no JS errors");
    } else {
        println!("main: {} JS error(s):", report.js_errors.len());
        for error in &report.js_errors {
            println!("  - {error}");
        }
    }
    println!("windows: {}", report.windows.len());
    for window in &report.windows {
        println!(
            "  #{} {}x{} entry={}",
            window.id, window.width, window.height, window.entry_file
        );
        match &window.page_title {
            Some(title) => println!("    title: {title}"),
            None => println!("    title: (none)"),
        }
        match &window.preload {
            Some(preload) => {
                if window.preload_errors.is_empty() {
                    println!("    preload: {preload} (ok)");
                } else {
                    println!(
                        "    preload: {preload} ({} error(s))",
                        window.preload_errors.len()
                    );
                    for error in &window.preload_errors {
                        println!("      - {error}");
                    }
                }
            }
            None => println!("    preload: (none)"),
        }
    }

    let failed = !report.js_errors.is_empty()
        || report
            .windows
            .iter()
            .any(|window| !window.preload_errors.is_empty());
    if failed {
        std::process::exit(1);
    }
}
